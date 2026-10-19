//! Regression guard for unconditional trait-adapter recursion.
//!
//! `impl Trait for Type { fn m(..) { Type::m(self, ..) } }` only delegates when `Type` has an
//! inherent `m`. A type that merely `Deref`s to an inner type (`ThreadManagementService` ->
//! `ThreadManagementCore`) has none, so `Type::m`, `Self::m` and `self.m` all resolve to the
//! trait method being defined and recurse until the stack overflows. That crashed the app on
//! launch whenever an imported thread had an interrupted run to continue.
//!
//! The guard is a source scan of every crate in the workspace: it fails on a trait-impl method
//! whose body calls a same-named method on its own type when the type has no inherent method of
//! that name. Delegate through the deref target instead (`std::ops::Deref::deref(self).m(..)`).

#[cfg(test)]
mod tests {
    use std::collections::{HashMap, HashSet};
    use std::path::{Path, PathBuf};

    fn rust_sources(dir: &Path, out: &mut Vec<PathBuf>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                rust_sources(&path, out);
            } else if path.extension().is_some_and(|ext| ext == "rs") {
                out.push(path);
            }
        }
    }

    /// Blanks comments, string/char literals (keeping newlines and length) so braces and
    /// method names inside them never confuse the scan.
    fn blank_literals(source: &str) -> Vec<char> {
        let chars: Vec<char> = source.chars().collect();
        let mut out = chars.clone();
        let blank = |out: &mut Vec<char>, from: usize, to: usize| {
            for slot in &mut out[from..to] {
                if *slot != '\n' {
                    *slot = ' ';
                }
            }
        };
        let is_ident = |c: char| c.is_alphanumeric() || c == '_';
        let mut i = 0;
        while i < chars.len() {
            let c = chars[i];
            let next = chars.get(i + 1).copied();
            if c == '/' && next == Some('/') {
                let end = chars[i..]
                    .iter()
                    .position(|&c| c == '\n')
                    .map_or(chars.len(), |p| i + p);
                blank(&mut out, i, end);
                i = end;
            } else if c == '/' && next == Some('*') {
                let mut depth = 0;
                let mut j = i;
                while j < chars.len() {
                    if chars[j] == '/' && chars.get(j + 1) == Some(&'*') {
                        depth += 1;
                        j += 2;
                    } else if chars[j] == '*' && chars.get(j + 1) == Some(&'/') {
                        depth -= 1;
                        j += 2;
                        if depth == 0 {
                            break;
                        }
                    } else {
                        j += 1;
                    }
                }
                blank(&mut out, i, j);
                i = j;
            } else if c == 'r' && (i == 0 || !is_ident(chars[i - 1])) && {
                let mut j = i + 1;
                while chars.get(j) == Some(&'#') {
                    j += 1;
                }
                chars.get(j) == Some(&'"')
            } {
                let mut hashes = 0;
                let mut j = i + 1;
                while chars[j] == '#' {
                    hashes += 1;
                    j += 1;
                }
                // `j` is the opening quote; the literal ends at `"` followed by the hashes.
                let mut k = j + 1;
                while k < chars.len() {
                    if chars[k] == '"' && chars[k + 1..].iter().take(hashes).all(|&c| c == '#') {
                        break;
                    }
                    k += 1;
                }
                let end = (k + 1 + hashes).min(chars.len());
                blank(&mut out, i, end);
                i = end;
            } else if c == '"' {
                let mut j = i + 1;
                while j < chars.len() && chars[j] != '"' {
                    j += if chars[j] == '\\' { 2 } else { 1 };
                }
                let end = (j + 1).min(chars.len());
                blank(&mut out, i, end);
                i = end;
            } else if c == '\'' {
                // A char literal (`'x'`, `'\n'`); anything else is a lifetime.
                let end = if next == Some('\\') {
                    chars[i + 2..]
                        .iter()
                        .position(|&c| c == '\'')
                        .map(|p| i + 2 + p + 1)
                } else if chars.get(i + 2) == Some(&'\'') {
                    Some(i + 3)
                } else {
                    None
                };
                match end {
                    Some(end) => {
                        blank(&mut out, i, end);
                        i = end;
                    }
                    None => i += 1,
                }
            } else {
                i += 1;
            }
        }
        out
    }

    fn is_ident(c: char) -> bool {
        c.is_alphanumeric() || c == '_'
    }

    fn word_at(text: &[char], at: usize, word: &str) -> bool {
        let end = at + word.chars().count();
        end <= text.len()
            && text[at..end].iter().copied().eq(word.chars())
            && (at == 0 || !is_ident(text[at - 1]))
            && text.get(end).is_none_or(|&c| !is_ident(c))
    }

    /// Index of the `}` matching the `{` at `open`.
    fn matching_brace(text: &[char], open: usize) -> usize {
        let mut depth = 0;
        for (offset, &c) in text[open..].iter().enumerate() {
            match c {
                '{' => depth += 1,
                '}' => {
                    depth -= 1;
                    if depth == 0 {
                        return open + offset;
                    }
                }
                _ => {}
            }
        }
        text.len().saturating_sub(1)
    }

    fn skip_generics(text: &[char], mut at: usize) -> usize {
        while text.get(at).is_some_and(|c| c.is_whitespace()) {
            at += 1;
        }
        if text.get(at) != Some(&'<') {
            return at;
        }
        let mut depth = 0;
        while at < text.len() {
            match text[at] {
                '<' => depth += 1,
                '>' if text.get(at.wrapping_sub(1)) != Some(&'-') => {
                    depth -= 1;
                    if depth == 0 {
                        return at + 1;
                    }
                }
                _ => {}
            }
            at += 1;
        }
        at
    }

    /// `Type` of `Path::Type<Args>` / `&Type` / `Arc<dyn X>` (-> `Arc`).
    fn simple_type_name(ty: &str) -> String {
        let ty = ty.trim().trim_start_matches('&').trim();
        let ty = ty.strip_prefix("mut ").unwrap_or(ty).trim();
        let ty = ty.split('<').next().unwrap_or(ty);
        ty.rsplit("::").next().unwrap_or(ty).trim().to_owned()
    }

    struct Impl {
        /// The self type's simple name.
        ty: String,
        /// The implemented trait's simple name; `None` for an inherent impl.
        trait_name: Option<String>,
        /// Methods defined directly in the block with their body span `(open, close)`.
        methods: Vec<(String, usize, usize)>,
    }

    fn parse_impls(text: &[char]) -> Vec<Impl> {
        let mut impls = Vec::new();
        let mut at = 0;
        while at < text.len() {
            if !word_at(text, at, "impl") {
                at += 1;
                continue;
            }
            // An item starts after `}`, `;`, `]` (attribute), `{` or the file start; `impl Trait`
            // in a type position follows `:`, `->`, `(` or `<` instead.
            let previous = text[..at].iter().rev().find(|c| !c.is_whitespace());
            if previous.is_some_and(|c| !matches!(c, '}' | ';' | ']' | '{')) {
                at += 4;
                continue;
            }
            let header_start = skip_generics(text, at + 4);
            let Some(open) = text[header_start..]
                .iter()
                .position(|&c| c == '{')
                .map(|p| header_start + p)
            else {
                break;
            };
            let header: String = text[header_start..open].iter().collect();
            let header = header.split(" where ").next().unwrap_or(&header);
            let (ty, trait_name) = match header.split_once(" for ") {
                Some((trait_name, ty)) => {
                    (simple_type_name(ty), Some(simple_type_name(trait_name)))
                }
                None => (simple_type_name(header), None),
            };
            let close = matching_brace(text, open);
            let mut methods = Vec::new();
            let mut depth = 0;
            let mut index = open;
            while index < close {
                match text[index] {
                    '{' => depth += 1,
                    '}' => depth -= 1,
                    _ if depth == 1 && word_at(text, index, "fn") => {
                        let mut name_at = index + 2;
                        while text[name_at].is_whitespace() {
                            name_at += 1;
                        }
                        let name: String = text[name_at..]
                            .iter()
                            .take_while(|&&c| is_ident(c))
                            .collect();
                        // The body opens at the first `{` before any `;` (a bodiless fn).
                        let rest = &text[name_at..close];
                        let body = rest.iter().position(|&c| c == '{' || c == ';');
                        if let Some(p) = body.filter(|&p| rest[p] == '{') {
                            let body_open = name_at + p;
                            let body_close = matching_brace(text, body_open);
                            methods.push((name, body_open, body_close));
                            index = body_close + 1;
                            continue;
                        }
                    }
                    _ => {}
                }
                index += 1;
            }
            impls.push(Impl {
                ty,
                trait_name,
                methods,
            });
            // Impl blocks nest only inside fn bodies; keep scanning inside this one.
            at = open + 1;
        }
        impls
    }

    /// Whether `body` calls `name` through `Type::name(`, `Self::name(` or `self.name(`.
    fn calls_own_method(body: &[char], ty: &str, name: &str) -> bool {
        let skip_ws = |mut at: usize| {
            while body.get(at).is_some_and(|c| c.is_whitespace()) {
                at += 1;
            }
            at
        };
        let followed_by_call = |at: usize| body.get(skip_ws(at)) == Some(&'(');
        for index in 0..body.len() {
            for owner in [ty, "Self"] {
                let path = format!("{owner}::{name}");
                let end = index + path.chars().count();
                if word_at(body, index, owner)
                    && end <= body.len()
                    && body[index..end].iter().copied().eq(path.chars())
                    && body.get(end).is_none_or(|&c| !is_ident(c))
                    && followed_by_call(end)
                {
                    return true;
                }
            }
            if word_at(body, index, "self") && body.get(index.wrapping_sub(1)) != Some(&'.') {
                let dot = skip_ws(index + 4);
                if body.get(dot) == Some(&'.') {
                    let start = skip_ws(dot + 1);
                    let end = start + name.chars().count();
                    if end <= body.len()
                        && body[start..end].iter().copied().eq(name.chars())
                        && body.get(end).is_none_or(|&c| !is_ident(c))
                        && followed_by_call(end)
                    {
                        return true;
                    }
                }
            }
        }
        false
    }

    /// Every `file:line` of a trait-impl method that calls its own same-named method on a type
    /// with no inherent method of that name.
    pub(super) fn offenders(files: &[(String, String)]) -> Vec<String> {
        let parsed: Vec<(&String, Vec<char>, Vec<Impl>)> = files
            .iter()
            .map(|(name, source)| {
                let text = blank_literals(source);
                let impls = parse_impls(&text);
                (name, text, impls)
            })
            .collect();
        let mut inherent: HashMap<&str, HashSet<&str>> = HashMap::new();
        for (_, _, impls) in &parsed {
            for block in impls.iter().filter(|block| block.trait_name.is_none()) {
                inherent
                    .entry(block.ty.as_str())
                    .or_default()
                    .extend(block.methods.iter().map(|(name, _, _)| name.as_str()));
            }
        }
        // `impl From<A> for T` next to `impl From<B> for T`: `Self::from(&b)` in one reaches the
        // other, so a method defined by several impls of one trait on a type is not flagged.
        let mut overloads: HashMap<(&str, &str, &str), usize> = HashMap::new();
        for (_, _, impls) in &parsed {
            for block in impls {
                let Some(trait_name) = &block.trait_name else {
                    continue;
                };
                for (name, _, _) in &block.methods {
                    *overloads
                        .entry((block.ty.as_str(), trait_name.as_str(), name.as_str()))
                        .or_default() += 1;
                }
            }
        }
        let mut found = Vec::new();
        for (file, text, impls) in &parsed {
            for block in impls {
                let Some(trait_name) = &block.trait_name else {
                    continue;
                };
                let own = inherent.get(block.ty.as_str());
                for (name, open, close) in &block.methods {
                    if own.is_some_and(|own| own.contains(name.as_str()))
                        || overloads[&(block.ty.as_str(), trait_name.as_str(), name.as_str())] > 1
                    {
                        continue;
                    }
                    if calls_own_method(&text[*open..=*close], &block.ty, name) {
                        let line = text[..*open].iter().filter(|&&c| c == '\n').count() + 1;
                        found.push(format!(
                            "{file}:{line}: `{}::{name}` in a trait impl calls itself \
                             (`{}` has no inherent `{name}`)",
                            block.ty, block.ty
                        ));
                    }
                }
            }
        }
        found
    }

    fn workspace_sources() -> Vec<(String, String)> {
        let crates = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .expect("crates dir");
        let mut files = Vec::new();
        for entry in std::fs::read_dir(crates).expect("crates dir").flatten() {
            rust_sources(&entry.path().join("src"), &mut files);
            rust_sources(&entry.path().join("tests"), &mut files);
        }
        files.sort();
        files
            .into_iter()
            .filter_map(|path| {
                let source = std::fs::read_to_string(&path).ok()?;
                let relative = path.strip_prefix(crates).unwrap_or(&path);
                Some((format!("crates/{}", relative.display()), source))
            })
            .collect()
    }

    fn scan(source: &str) -> Vec<String> {
        offenders(&[("fixture.rs".to_owned(), source.to_owned())])
    }

    #[test]
    fn no_trait_impl_method_recurses_into_itself() {
        let sources = workspace_sources();
        assert!(sources.len() > 100, "workspace scan found no sources");
        let found = offenders(&sources);
        assert!(
            found.is_empty(),
            "trait-impl methods that resolve to themselves and overflow the stack \
             (delegate through the deref target, e.g. `std::ops::Deref::deref(self).m(..)`):\n{}",
            found.join("\n")
        );
    }

    /// The shape that crashed the app on launch: the service has no inherent `dispatch`
    /// (it derefs to the core), so `Service::dispatch(self, ..)` is the trait method itself.
    #[test]
    fn the_deref_adapter_recursion_is_detected() {
        let found = scan(
            r#"
pub struct Service { core: Arc<Core> }
impl Deref for Service {
    type Target = Core;
    fn deref(&self) -> &Core { &self.core }
}
#[async_trait]
impl RestartThreads for Service {
    async fn dispatch(&self, command: Command) -> Result<(), Error> {
        Service::dispatch(self, command).await
    }
    async fn send(&self, input: Input) -> Result<(), Error> {
        self.send(input).await
    }
    async fn other(&self) {
        Self::other(self).await
    }
}
"#,
        );
        assert_eq!(found.len(), 3, "{found:?}");
        assert!(found[0].starts_with("fixture.rs:9: `Service::dispatch`"));
        assert!(found[1].starts_with("fixture.rs:12: `Service::send`"));
        assert!(found[2].starts_with("fixture.rs:15: `Service::other`"));
    }

    #[test]
    fn overloaded_trait_impls_may_call_each_other() {
        let found = scan(
            r#"
impl From<&Ref> for Target {
    fn from(request: &Ref) -> Self { Self { n: request.n } }
}
impl From<Ref> for Target {
    fn from(request: Ref) -> Self { Self::from(&request) }
}
"#,
        );
        assert!(found.is_empty(), "{found:?}");
    }

    #[test]
    fn delegation_that_does_not_recurse_passes() {
        let found = scan(
            r#"
impl Core {
    pub async fn dispatch(&self, command: Command) {}
}
impl Service {
    pub async fn inherent_here(&self) {}
}
#[async_trait]
impl RestartThreads for Core {
    // Core has an inherent `dispatch`, so this reaches it.
    async fn dispatch(&self, command: Command) { Core::dispatch(self, command).await }
}
#[async_trait]
impl RestartThreads for Service {
    async fn dispatch(&self, command: Command) {
        std::ops::Deref::deref(self).dispatch(command).await
    }
    async fn send(&self, input: Input) {
        self.core.send(input).await;
        <Core as Sender>::send(&self.core, input).await;
        let text = "Service::send(self)";
    }
    async fn inherent_here(&self) {
        // `Service::inherent_here(self)` is a comment, and the method exists inherently.
    }
}
"#,
        );
        assert!(found.is_empty(), "{found:?}");
    }
}
