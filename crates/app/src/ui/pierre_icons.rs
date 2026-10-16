//! Port of upstream/apps/web/src/pierre-icons.ts (+ test) and the file-icon
//! resolution of `@pierre/trees` it extends (`createFileTreeIconResolver`,
//! `builtInIcons.ts`, the "complete" set).
//!
//! The resolver maps a file path (and, for an extensionless file, its detected
//! MIME type) to an icon name plus the token that tints it. The names are
//! `file-tree-builtin-<token>` for Pierre's own icons and `t3-file-icon-*` for
//! the ones T3 adds. Upstream injects an inline `<svg><symbol>` sprite into
//! the document (`ensurePierreIconSprite`) and tints through CSS; here each
//! symbol is its own SVG asset under `assets/pierre/` (`pierre_icon_asset`),
//! and the tint is [`pierre_icon_token_color`]. See PORTING.md Deviations.
//!
//! The tables below are copied from `@pierre/trees` (the resolver and
//! `builtInIcons`) and from `pierre-icons.ts`; the SVG assets are its sprite
//! symbols, one file each. Symbols that tint with `light-dark()` ship a
//! `.light.svg` and a `.dark.svg` (GPUI's SVG renderer has no `light-dark()`).
// The changed-files tree, diff file tree and file previews take their icons
// from here as their views are wired up.
#![allow(dead_code)]

use turin_shared::file_preview::file_icon_path_for_mime_type;
use turin_shared::video::video_file_extensions;

/// `PierreIconResolution`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PierreIconResolution {
    pub name: String,
    pub token: Option<String>,
}

/// `T3_PIERRE_ICONS.byFileName` (keys lower-cased, as the resolver does).
const T3_BY_FILE_NAME: &[(&str, &str)] = &[
    ("package.json", "file-tree-builtin-npm"),
    ("tsconfig.json", "file-tree-builtin-typescript"),
    ("agents.md", "t3-file-icon-agents"),
    ("pnpm-lock.yaml", "t3-file-icon-pnpm"),
    ("pnpm-workspace.yaml", "t3-file-icon-pnpm"),
];

/// `T3_PIERRE_ICONS.byFileExtension`, after the video extensions: "languages
/// the complete set has no icon for".
const T3_BY_FILE_EXTENSION: &[(&str, &str)] = &[
    ("cs", "t3-file-icon-csharp"),
    ("csx", "t3-file-icon-csharp"),
    ("dart", "t3-file-icon-dart"),
    ("diff", "t3-file-icon-diff"),
    ("patch", "t3-file-icon-diff"),
    ("ex", "t3-file-icon-elixir"),
    ("exs", "t3-file-icon-elixir"),
    ("hs", "t3-file-icon-haskell"),
    ("lhs", "t3-file-icon-haskell"),
    ("java", "t3-file-icon-java"),
    ("kt", "t3-file-icon-kotlin"),
    ("kts", "t3-file-icon-kotlin"),
    ("lua", "t3-file-icon-lua"),
    ("php", "t3-file-icon-php"),
    ("ps1", "t3-file-icon-powershell"),
    ("psm1", "t3-file-icon-powershell"),
    ("psd1", "t3-file-icon-powershell"),
    ("r", "t3-file-icon-r"),
    ("scala", "t3-file-icon-scala"),
    ("toml", "t3-file-icon-toml"),
    ("xml", "t3-file-icon-xml"),
];

const T3_VIDEO_ICON: &str = "t3-file-icon-video";

/// Tokens the "standard" tier ships. The app uses the "complete" set, which
/// accepts every token; kept for the tier rule in [`resolve_built_in_file_icon_token`].
const STANDARD_TIER_TOKENS: &[&str] = &[
    "bash",
    "c",
    "cpp",
    "css",
    "database",
    "default",
    "font",
    "git",
    "go",
    "html",
    "image",
    "javascript",
    "json",
    "markdown",
    "mcp",
    "python",
    "ruby",
    "rust",
    "swift",
    "table",
    "text",
    "typescript",
    "zip",
];

/// The icon sets of `@pierre/trees`. T3 always uses `Complete`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BuiltInIconSet {
    None,
    Minimal,
    Standard,
    Complete,
}

fn lookup(table: &'static [(&'static str, &'static str)], key: &str) -> Option<&'static str> {
    table
        .iter()
        .find(|(candidate, _)| *candidate == key)
        .map(|(_, value)| *value)
}

/// `getExtensionCandidates`: `a.b.c` gives `b.c`, then `c`.
fn extension_candidates(file_name: &str) -> Vec<String> {
    let lowered = file_name.to_lowercase();
    let segments: Vec<&str> = lowered.split('.').collect();
    (1..segments.len())
        .map(|index| segments[index..].join("."))
        .collect()
}

/// `resolveBuiltInFileIconToken`: `None` for the minimal and none sets.
fn resolve_built_in_file_icon_token(
    set: BuiltInIconSet,
    file_name: &str,
    candidates: &[String],
) -> Option<&'static str> {
    if matches!(set, BuiltInIconSet::Minimal | BuiltInIconSet::None) {
        return None;
    }
    let is_complete = set == BuiltInIconSet::Complete;
    if let Some(token) = lookup(BUILT_IN_FILE_NAME_TOKENS, &file_name.to_lowercase())
        && (is_complete || STANDARD_TIER_TOKENS.contains(&token))
    {
        return Some(token);
    }
    for extension in candidates {
        if is_complete && let Some(token) = lookup(COMPLETE_EXTENSION_OVERRIDES, extension) {
            return Some(token);
        }
        if let Some(token) = lookup(BUILT_IN_FILE_EXTENSION_TOKENS, extension)
            && (is_complete || STANDARD_TIER_TOKENS.contains(&token))
        {
            return Some(token);
        }
    }
    Some("default")
}

/// `getBuiltInFileIconName`.
pub fn built_in_file_icon_name(token: &str) -> String {
    format!("file-tree-builtin-{token}")
}

fn base_file_name(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

/// `completeIconResolver.resolveIcon("file-tree-icon-file", path)`: the T3
/// file-name rules, then the T3 extension rules (video extensions included),
/// then Pierre's built-in "complete" set.
fn resolve_complete_file_icon(path: &str) -> PierreIconResolution {
    let file_name = base_file_name(path);
    let lower_file_name = file_name.to_lowercase();
    if let Some(name) = lookup(T3_BY_FILE_NAME, &lower_file_name) {
        return PierreIconResolution {
            name: name.to_owned(),
            token: None,
        };
    }
    let candidates = extension_candidates(file_name);
    let video_extensions = video_file_extensions();
    for extension in &candidates {
        let custom = if video_extensions.contains(&extension.as_str()) {
            Some(T3_VIDEO_ICON)
        } else {
            lookup(T3_BY_FILE_EXTENSION, extension)
        };
        if let Some(name) = custom {
            return PierreIconResolution {
                name: name.to_owned(),
                token: None,
            };
        }
    }
    let token = resolve_built_in_file_icon_token(BuiltInIconSet::Complete, file_name, &candidates)
        .unwrap_or("default");
    PierreIconResolution {
        name: built_in_file_icon_name(token),
        token: Some(token.to_owned()),
    }
}

const LANGUAGE_EXTENSION_ALIASES: &[(&str, &str)] = &[
    ("bash", "sh"),
    ("csharp", "cs"),
    ("elixir", "ex"),
    ("haskell", "hs"),
    ("javascript", "js"),
    ("jsx", "jsx"),
    ("kotlin", "kt"),
    ("markdown", "md"),
    ("mdx", "mdx"),
    ("plaintext", "txt"),
    ("powershell", "ps1"),
    ("python", "py"),
    ("ruby", "rb"),
    ("rust", "rs"),
    ("shell", "sh"),
    ("shellscript", "sh"),
    ("swift", "swift"),
    ("typescript", "ts"),
    ("tsx", "tsx"),
    ("yaml", "yml"),
];

/// Languages whose files are recognised by name rather than by extension.
const LANGUAGE_FILE_NAMES: &[(&str, &str)] = &[("dockerfile", "Dockerfile")];

/// `basenameOfPath`: the part after the last `/`.
pub fn basename_of_path(path_value: &str) -> &str {
    base_file_name(path_value)
}

pub fn synthetic_file_name_for_language_id(language_id: &str) -> String {
    let normalized = language_id.to_lowercase();
    if let Some(name) = lookup(LANGUAGE_FILE_NAMES, &normalized) {
        return name.to_owned();
    }
    let extension = lookup(LANGUAGE_EXTENSION_ALIASES, &normalized).unwrap_or(&normalized);
    format!("file.{extension}")
}

/// `resolvePierreIconForEntry`: `None` for a directory, which the shared
/// folder fallback draws. A detected MIME type fills in for a file whose name
/// resolves to the default icon.
pub fn resolve_pierre_icon_for_entry(
    path_value: &str,
    is_directory: bool,
    mime_type: Option<&str>,
) -> Option<PierreIconResolution> {
    if is_directory {
        return None;
    }
    let icon = resolve_complete_file_icon(path_value);
    match mime_type.filter(|mime| !mime.is_empty()) {
        Some(mime_type) if icon.token.as_deref() == Some("default") => Some(
            resolve_complete_file_icon(&file_icon_path_for_mime_type(path_value, Some(mime_type))),
        ),
        _ => Some(icon),
    }
}

pub fn has_specific_pierre_icon_for_file_name(file_name: &str) -> bool {
    resolve_pierre_icon_for_entry(file_name, false, None)
        .and_then(|icon| icon.token)
        .as_deref()
        != Some("default")
}

/// The palette `@pierre/trees` tints icons with (`--trees-icon-*`), as
/// (light, dark) sRGB hex.
fn palette(name: &str) -> Option<(u32, u32)> {
    Some(match name {
        "blue" => (0x1a85d4, 0x69b1ff),
        "cyan" => (0x1ca1c7, 0x68cdf2),
        "gray" => (0x84848a, 0xadadb1),
        "green" => (0x199f43, 0x5ecc71),
        "indigo" => (0x693acf, 0x9d6afb),
        "mauve" => (0x594c5b, 0x79697b),
        "orange" => (0xd47628, 0xffa359),
        "pink" => (0xd32a61, 0xff678d),
        "purple" => (0xa631be, 0xd568ea),
        "red" => (0xd52c36, 0xff6762),
        "teal" => (0x17a5af, 0x64d1db),
        "vermilion" => (0xff8c5b, 0xd5512f),
        "yellow" => (0xd5a910, 0xffd452),
        _ => return None,
    })
}

/// `--trees-file-icon-color-<token>` as sRGB hex for the light or dark scheme.
/// `None` for a token Pierre does not tint (it keeps the muted foreground).
pub fn pierre_icon_token_color(token: &str, dark: bool) -> Option<u32> {
    let name = lookup(TOKEN_COLORS, token)?;
    let (light, dark_value) = palette(name)?;
    Some(if dark { dark_value } else { light })
}

/// The SVG for an icon name returned by the resolver. `dark` selects the
/// variant of a symbol that tints with `light-dark()`.
pub fn pierre_icon_asset(name: &str, dark: bool) -> Option<&'static [u8]> {
    let scheme = if dark { "dark" } else { "light" };
    ICON_FILES
        .iter()
        .find(|(id, variant, _)| *id == name && variant.is_none_or(|variant| variant == scheme))
        .map(|(_, _, bytes)| *bytes)
}

/// The asset path (as AppAssets.load receives it) for an icon file.
pub fn pierre_icon_asset_path(name: &str, dark: bool) -> Option<String> {
    let scheme = if dark { "dark" } else { "light" };
    ICON_FILES
        .iter()
        .find(|(id, variant, _)| *id == name && variant.is_none_or(|variant| variant == scheme))
        .map(|(id, variant, _)| match variant {
            Some(variant) => format!("pierre/{id}.{variant}.svg"),
            None => format!("pierre/{id}.svg"),
        })
}

/// `AppAssets.load` for `pierre/<id>[.light|.dark].svg`.
pub fn load_pierre_icon(path: &str) -> Option<&'static [u8]> {
    let file = path.strip_prefix("pierre/")?.strip_suffix(".svg")?;
    let (id, scheme) = match file.rsplit_once('.') {
        Some((id, scheme @ ("light" | "dark"))) => (id, Some(scheme)),
        _ => (file, None),
    };
    ICON_FILES
        .iter()
        .find(|(candidate, variant, _)| *candidate == id && *variant == scheme)
        .map(|(_, _, bytes)| *bytes)
}

/// Every asset path this module serves.
pub fn pierre_icon_asset_paths() -> Vec<String> {
    ICON_FILES
        .iter()
        .map(|(id, variant, _)| match variant {
            Some(variant) => format!("pierre/{id}.{variant}.svg"),
            None => format!("pierre/{id}.svg"),
        })
        .collect()
}

/// `BUILT_IN_FILE_NAME_TOKENS` (@pierre/trees `builtInIcons.ts`): lower-cased file names.
const BUILT_IN_FILE_NAME_TOKENS: &[(&str, &str)] = &[
    (".babelrc", "babel"),
    (".babelrc.json", "babel"),
    (".bash_profile", "bash"),
    (".bashrc", "bash"),
    (".browserslistrc", "browserslist"),
    (".dockerignore", "docker"),
    (".eslintignore", "eslint"),
    (".eslintrc", "eslint"),
    (".eslintrc.cjs", "eslint"),
    (".eslintrc.js", "eslint"),
    (".eslintrc.json", "eslint"),
    (".eslintrc.yaml", "eslint"),
    (".eslintrc.yml", "eslint"),
    (".gitattributes", "git"),
    (".gitignore", "git"),
    (".gitkeep", "git"),
    (".gitmodules", "git"),
    (".oxlintrc.json", "oxc"),
    (".postcssrc", "postcss"),
    (".postcssrc.json", "postcss"),
    (".postcssrc.yaml", "postcss"),
    (".postcssrc.yml", "postcss"),
    (".prettierignore", "prettier"),
    (".prettierrc", "prettier"),
    (".prettierrc.cjs", "prettier"),
    (".prettierrc.js", "prettier"),
    (".prettierrc.json", "prettier"),
    (".prettierrc.mjs", "prettier"),
    (".prettierrc.toml", "prettier"),
    (".prettierrc.yaml", "prettier"),
    (".prettierrc.yml", "prettier"),
    (".stylelintignore", "stylelint"),
    (".stylelintrc", "stylelint"),
    (".stylelintrc.cjs", "stylelint"),
    (".stylelintrc.js", "stylelint"),
    (".stylelintrc.json", "stylelint"),
    (".stylelintrc.mjs", "stylelint"),
    (".stylelintrc.yaml", "stylelint"),
    (".stylelintrc.yml", "stylelint"),
    (".terraform.lock.hcl", "terraform"),
    (".zprofile", "bash"),
    (".zshenv", "bash"),
    (".zshrc", "bash"),
    ("babel.config.cjs", "babel"),
    ("babel.config.js", "babel"),
    ("babel.config.json", "babel"),
    ("babel.config.mjs", "babel"),
    ("biome.json", "biome"),
    ("biome.jsonc", "biome"),
    ("bootstrap.bundle.js", "bootstrap"),
    ("bootstrap.bundle.min.js", "bootstrap"),
    ("bootstrap.css", "bootstrap"),
    ("bootstrap.js", "bootstrap"),
    ("bootstrap.min.css", "bootstrap"),
    ("bootstrap.min.js", "bootstrap"),
    ("bun.lock", "bun"),
    ("bun.lockb", "bun"),
    ("bunfig.toml", "bun"),
    ("claude.md", "claude"),
    ("compose.yaml", "docker"),
    ("compose.yml", "docker"),
    ("docker-compose.override.yml", "docker"),
    ("docker-compose.yaml", "docker"),
    ("docker-compose.yml", "docker"),
    ("dockerfile", "docker"),
    ("eslint.config.cjs", "eslint"),
    ("eslint.config.js", "eslint"),
    ("eslint.config.mjs", "eslint"),
    ("eslint.config.mts", "eslint"),
    ("eslint.config.ts", "eslint"),
    ("gemfile", "ruby"),
    ("next.config.js", "nextjs"),
    ("next.config.mjs", "nextjs"),
    ("next.config.mts", "nextjs"),
    ("next.config.ts", "nextjs"),
    ("postcss.config.cjs", "postcss"),
    ("postcss.config.js", "postcss"),
    ("postcss.config.mjs", "postcss"),
    ("postcss.config.ts", "postcss"),
    ("prettier.config.cjs", "prettier"),
    ("prettier.config.js", "prettier"),
    ("prettier.config.mjs", "prettier"),
    ("rakefile", "ruby"),
    ("readme.md", "markdown"),
    ("stylelint.config.cjs", "stylelint"),
    ("stylelint.config.js", "stylelint"),
    ("stylelint.config.mjs", "stylelint"),
    ("svgo.config.cjs", "svgo"),
    ("svgo.config.js", "svgo"),
    ("svgo.config.mjs", "svgo"),
    ("svgo.config.ts", "svgo"),
    ("tailwind.config.cjs", "tailwind"),
    ("tailwind.config.js", "tailwind"),
    ("tailwind.config.mjs", "tailwind"),
    ("tailwind.config.ts", "tailwind"),
    ("vite.config.js", "vite"),
    ("vite.config.mjs", "vite"),
    ("vite.config.mts", "vite"),
    ("vite.config.ts", "vite"),
    ("webpack.config.babel.js", "webpack"),
    ("webpack.config.cjs", "webpack"),
    ("webpack.config.js", "webpack"),
    ("webpack.config.mjs", "webpack"),
    ("webpack.config.ts", "webpack"),
];

/// `BUILT_IN_FILE_EXTENSION_TOKENS`. Upstream's keys `AUTHORS`, `CHANGELOG`, `CONTRIBUTORS` and
/// `LICENSE` are upper case, and candidates are lower-cased, so they never match; kept as they are.
const BUILT_IN_FILE_EXTENSION_TOKENS: &[(&str, &str)] = &[
    ("7z", "zip"),
    ("astro", "astro"),
    ("AUTHORS", "text"),
    ("avif", "image"),
    ("bash", "bash"),
    ("bmp", "image"),
    ("bz2", "zip"),
    ("c", "c"),
    ("cc", "cpp"),
    ("cfg", "text"),
    ("CHANGELOG", "text"),
    ("cjs", "javascript"),
    ("code-workspace", "vscode"),
    ("conf", "text"),
    ("CONTRIBUTORS", "text"),
    ("cpp", "cpp"),
    ("csh", "bash"),
    ("css", "css"),
    ("csv", "table"),
    ("cts", "typescript"),
    ("cxx", "cpp"),
    ("db", "database"),
    ("editorconfig", "text"),
    ("env", "text"),
    ("env.development", "text"),
    ("env.local", "text"),
    ("env.production", "text"),
    ("eot", "font"),
    ("erb", "ruby"),
    ("fish", "bash"),
    ("gemspec", "ruby"),
    ("gif", "image"),
    ("go", "go"),
    ("gql", "graphql"),
    ("graphql", "graphql"),
    ("gz", "zip"),
    ("h", "c"),
    ("hh", "cpp"),
    ("hpp", "cpp"),
    ("htm", "html"),
    ("html", "html"),
    ("hxx", "cpp"),
    ("icns", "image"),
    ("ico", "image"),
    ("ini", "text"),
    ("inl", "cpp"),
    ("jar", "zip"),
    ("jpeg", "image"),
    ("jpg", "image"),
    ("js", "javascript"),
    ("json", "json"),
    ("json5", "json"),
    ("jsonc", "json"),
    ("jsonl", "json"),
    ("jsx", "javascript"),
    ("ksh", "bash"),
    ("less", "css"),
    ("LICENSE", "text"),
    ("log", "text"),
    ("markdown", "markdown"),
    ("mcp", "mcp"),
    ("md", "markdown"),
    ("mdx", "markdown"),
    ("mdx.tsx", "markdown"),
    ("mjs", "javascript"),
    ("mm", "cpp"),
    ("mts", "typescript"),
    ("ods", "table"),
    ("otf", "font"),
    ("png", "image"),
    ("postcss", "css"),
    ("py", "python"),
    ("pyi", "python"),
    ("pyw", "python"),
    ("pyx", "python"),
    ("rake", "ruby"),
    ("rar", "zip"),
    ("rb", "ruby"),
    ("rs", "rust"),
    ("rst", "text"),
    ("rtf", "text"),
    ("sass", "css"),
    ("scss", "css"),
    ("sh", "bash"),
    ("sql", "database"),
    ("sqlite", "database"),
    ("sqlite3", "database"),
    ("styl", "css"),
    ("svelte", "svelte"),
    ("svg", "svg"),
    ("swift", "swift"),
    ("tar", "zip"),
    ("tf", "terraform"),
    ("tfstate", "terraform"),
    ("tfvars", "terraform"),
    ("tgz", "zip"),
    ("tif", "image"),
    ("tiff", "image"),
    ("ts", "typescript"),
    ("tsv", "table"),
    ("tsx", "typescript"),
    ("ttf", "font"),
    ("txt", "text"),
    ("vue", "vue"),
    ("war", "zip"),
    ("wasm", "wasm"),
    ("wast", "wasm"),
    ("wat", "wasm"),
    ("webp", "image"),
    ("woff", "font"),
    ("woff2", "font"),
    ("xhtml", "html"),
    ("xls", "table"),
    ("xlsx", "table"),
    ("xz", "zip"),
    ("yaml", "yml"),
    ("yml", "yml"),
    ("zig", "zig"),
    ("zip", "zip"),
    ("zsh", "bash"),
];

/// `COMPLETE_EXTENSION_OVERRIDES`.
const COMPLETE_EXTENSION_OVERRIDES: &[(&str, &str)] = &[
    ("jsx", "react"),
    ("sass", "sass"),
    ("scss", "sass"),
    ("tsx", "react"),
];

/// `--trees-file-icon-color-<token>`: the palette name each token tints with.
const TOKEN_COLORS: &[(&str, &str)] = &[
    ("astro", "purple"),
    ("babel", "yellow"),
    ("bash", "green"),
    ("biome", "blue"),
    ("bootstrap", "indigo"),
    ("browserslist", "yellow"),
    ("bun", "mauve"),
    ("c", "blue"),
    ("claude", "orange"),
    ("cpp", "blue"),
    ("css", "indigo"),
    ("database", "purple"),
    ("default", "gray"),
    ("docker", "blue"),
    ("eslint", "indigo"),
    ("git", "vermilion"),
    ("go", "cyan"),
    ("graphql", "pink"),
    ("html", "orange"),
    ("image", "pink"),
    ("javascript", "yellow"),
    ("json", "orange"),
    ("markdown", "green"),
    ("mcp", "teal"),
    ("npm", "red"),
    ("oxc", "cyan"),
    ("postcss", "red"),
    ("prettier", "teal"),
    ("python", "blue"),
    ("react", "cyan"),
    ("ruby", "red"),
    ("rust", "orange"),
    ("sass", "pink"),
    ("svelte", "red"),
    ("svg", "orange"),
    ("svgo", "green"),
    ("swift", "orange"),
    ("table", "teal"),
    ("tailwind", "cyan"),
    ("terraform", "indigo"),
    ("text", "gray"),
    ("typescript", "blue"),
    ("vite", "purple"),
    ("vscode", "blue"),
    ("vue", "green"),
    ("wasm", "indigo"),
    ("webpack", "blue"),
    ("yml", "red"),
    ("zig", "orange"),
    ("zip", "orange"),
];

/// Every shipped icon: (symbol id, light/dark variant, file name).
const ICON_FILES: &[(&str, Option<&str>, &[u8])] = &[
    (
        "file-tree-builtin-astro",
        None,
        include_bytes!("../../assets/pierre/file-tree-builtin-astro.svg"),
    ),
    (
        "file-tree-builtin-babel",
        None,
        include_bytes!("../../assets/pierre/file-tree-builtin-babel.svg"),
    ),
    (
        "file-tree-builtin-bash",
        None,
        include_bytes!("../../assets/pierre/file-tree-builtin-bash.svg"),
    ),
    (
        "file-tree-builtin-biome",
        None,
        include_bytes!("../../assets/pierre/file-tree-builtin-biome.svg"),
    ),
    (
        "file-tree-builtin-bootstrap",
        None,
        include_bytes!("../../assets/pierre/file-tree-builtin-bootstrap.svg"),
    ),
    (
        "file-tree-builtin-browserslist",
        None,
        include_bytes!("../../assets/pierre/file-tree-builtin-browserslist.svg"),
    ),
    (
        "file-tree-builtin-bun",
        None,
        include_bytes!("../../assets/pierre/file-tree-builtin-bun.svg"),
    ),
    (
        "file-tree-builtin-c",
        None,
        include_bytes!("../../assets/pierre/file-tree-builtin-c.svg"),
    ),
    (
        "file-tree-builtin-claude",
        None,
        include_bytes!("../../assets/pierre/file-tree-builtin-claude.svg"),
    ),
    (
        "file-tree-builtin-cpp",
        None,
        include_bytes!("../../assets/pierre/file-tree-builtin-cpp.svg"),
    ),
    (
        "file-tree-builtin-css",
        None,
        include_bytes!("../../assets/pierre/file-tree-builtin-css.svg"),
    ),
    (
        "file-tree-builtin-database",
        None,
        include_bytes!("../../assets/pierre/file-tree-builtin-database.svg"),
    ),
    (
        "file-tree-builtin-default",
        None,
        include_bytes!("../../assets/pierre/file-tree-builtin-default.svg"),
    ),
    (
        "file-tree-builtin-docker",
        None,
        include_bytes!("../../assets/pierre/file-tree-builtin-docker.svg"),
    ),
    (
        "file-tree-builtin-eslint",
        None,
        include_bytes!("../../assets/pierre/file-tree-builtin-eslint.svg"),
    ),
    (
        "file-tree-builtin-font",
        None,
        include_bytes!("../../assets/pierre/file-tree-builtin-font.svg"),
    ),
    (
        "file-tree-builtin-git",
        None,
        include_bytes!("../../assets/pierre/file-tree-builtin-git.svg"),
    ),
    (
        "file-tree-builtin-go",
        None,
        include_bytes!("../../assets/pierre/file-tree-builtin-go.svg"),
    ),
    (
        "file-tree-builtin-graphql",
        None,
        include_bytes!("../../assets/pierre/file-tree-builtin-graphql.svg"),
    ),
    (
        "file-tree-builtin-html",
        None,
        include_bytes!("../../assets/pierre/file-tree-builtin-html.svg"),
    ),
    (
        "file-tree-builtin-image",
        None,
        include_bytes!("../../assets/pierre/file-tree-builtin-image.svg"),
    ),
    (
        "file-tree-builtin-javascript",
        None,
        include_bytes!("../../assets/pierre/file-tree-builtin-javascript.svg"),
    ),
    (
        "file-tree-builtin-json",
        None,
        include_bytes!("../../assets/pierre/file-tree-builtin-json.svg"),
    ),
    (
        "file-tree-builtin-markdown",
        None,
        include_bytes!("../../assets/pierre/file-tree-builtin-markdown.svg"),
    ),
    (
        "file-tree-builtin-mcp",
        None,
        include_bytes!("../../assets/pierre/file-tree-builtin-mcp.svg"),
    ),
    (
        "file-tree-builtin-nextjs",
        None,
        include_bytes!("../../assets/pierre/file-tree-builtin-nextjs.svg"),
    ),
    (
        "file-tree-builtin-npm",
        None,
        include_bytes!("../../assets/pierre/file-tree-builtin-npm.svg"),
    ),
    (
        "file-tree-builtin-oxc",
        None,
        include_bytes!("../../assets/pierre/file-tree-builtin-oxc.svg"),
    ),
    (
        "file-tree-builtin-postcss",
        None,
        include_bytes!("../../assets/pierre/file-tree-builtin-postcss.svg"),
    ),
    (
        "file-tree-builtin-prettier",
        None,
        include_bytes!("../../assets/pierre/file-tree-builtin-prettier.svg"),
    ),
    (
        "file-tree-builtin-python",
        None,
        include_bytes!("../../assets/pierre/file-tree-builtin-python.svg"),
    ),
    (
        "file-tree-builtin-react",
        None,
        include_bytes!("../../assets/pierre/file-tree-builtin-react.svg"),
    ),
    (
        "file-tree-builtin-ruby",
        None,
        include_bytes!("../../assets/pierre/file-tree-builtin-ruby.svg"),
    ),
    (
        "file-tree-builtin-rust",
        None,
        include_bytes!("../../assets/pierre/file-tree-builtin-rust.svg"),
    ),
    (
        "file-tree-builtin-sass",
        None,
        include_bytes!("../../assets/pierre/file-tree-builtin-sass.svg"),
    ),
    (
        "file-tree-builtin-stylelint",
        None,
        include_bytes!("../../assets/pierre/file-tree-builtin-stylelint.svg"),
    ),
    (
        "file-tree-builtin-svelte",
        None,
        include_bytes!("../../assets/pierre/file-tree-builtin-svelte.svg"),
    ),
    (
        "file-tree-builtin-svg",
        None,
        include_bytes!("../../assets/pierre/file-tree-builtin-svg.svg"),
    ),
    (
        "file-tree-builtin-svgo",
        None,
        include_bytes!("../../assets/pierre/file-tree-builtin-svgo.svg"),
    ),
    (
        "file-tree-builtin-swift",
        None,
        include_bytes!("../../assets/pierre/file-tree-builtin-swift.svg"),
    ),
    (
        "file-tree-builtin-table",
        None,
        include_bytes!("../../assets/pierre/file-tree-builtin-table.svg"),
    ),
    (
        "file-tree-builtin-tailwind",
        None,
        include_bytes!("../../assets/pierre/file-tree-builtin-tailwind.svg"),
    ),
    (
        "file-tree-builtin-terraform",
        None,
        include_bytes!("../../assets/pierre/file-tree-builtin-terraform.svg"),
    ),
    (
        "file-tree-builtin-text",
        None,
        include_bytes!("../../assets/pierre/file-tree-builtin-text.svg"),
    ),
    (
        "file-tree-builtin-typescript",
        None,
        include_bytes!("../../assets/pierre/file-tree-builtin-typescript.svg"),
    ),
    (
        "file-tree-builtin-vite",
        None,
        include_bytes!("../../assets/pierre/file-tree-builtin-vite.svg"),
    ),
    (
        "file-tree-builtin-vscode",
        None,
        include_bytes!("../../assets/pierre/file-tree-builtin-vscode.svg"),
    ),
    (
        "file-tree-builtin-vue",
        None,
        include_bytes!("../../assets/pierre/file-tree-builtin-vue.svg"),
    ),
    (
        "file-tree-builtin-wasm",
        None,
        include_bytes!("../../assets/pierre/file-tree-builtin-wasm.svg"),
    ),
    (
        "file-tree-builtin-webpack",
        None,
        include_bytes!("../../assets/pierre/file-tree-builtin-webpack.svg"),
    ),
    (
        "file-tree-builtin-yml",
        None,
        include_bytes!("../../assets/pierre/file-tree-builtin-yml.svg"),
    ),
    (
        "file-tree-builtin-zig",
        None,
        include_bytes!("../../assets/pierre/file-tree-builtin-zig.svg"),
    ),
    (
        "file-tree-builtin-zip",
        None,
        include_bytes!("../../assets/pierre/file-tree-builtin-zip.svg"),
    ),
    (
        "t3-file-icon-video",
        None,
        include_bytes!("../../assets/pierre/t3-file-icon-video.svg"),
    ),
    (
        "t3-file-icon-agents",
        None,
        include_bytes!("../../assets/pierre/t3-file-icon-agents.svg"),
    ),
    (
        "t3-file-icon-pnpm",
        None,
        include_bytes!("../../assets/pierre/t3-file-icon-pnpm.svg"),
    ),
    (
        "t3-tree-icon-loading",
        None,
        include_bytes!("../../assets/pierre/t3-tree-icon-loading.svg"),
    ),
    (
        "t3-file-icon-diff",
        Some("light"),
        include_bytes!("../../assets/pierre/t3-file-icon-diff.light.svg"),
    ),
    (
        "t3-file-icon-diff",
        Some("dark"),
        include_bytes!("../../assets/pierre/t3-file-icon-diff.dark.svg"),
    ),
    (
        "t3-file-icon-xml",
        Some("light"),
        include_bytes!("../../assets/pierre/t3-file-icon-xml.light.svg"),
    ),
    (
        "t3-file-icon-xml",
        Some("dark"),
        include_bytes!("../../assets/pierre/t3-file-icon-xml.dark.svg"),
    ),
    (
        "t3-file-icon-csharp",
        Some("light"),
        include_bytes!("../../assets/pierre/t3-file-icon-csharp.light.svg"),
    ),
    (
        "t3-file-icon-csharp",
        Some("dark"),
        include_bytes!("../../assets/pierre/t3-file-icon-csharp.dark.svg"),
    ),
    (
        "t3-file-icon-dart",
        Some("light"),
        include_bytes!("../../assets/pierre/t3-file-icon-dart.light.svg"),
    ),
    (
        "t3-file-icon-dart",
        Some("dark"),
        include_bytes!("../../assets/pierre/t3-file-icon-dart.dark.svg"),
    ),
    (
        "t3-file-icon-elixir",
        Some("light"),
        include_bytes!("../../assets/pierre/t3-file-icon-elixir.light.svg"),
    ),
    (
        "t3-file-icon-elixir",
        Some("dark"),
        include_bytes!("../../assets/pierre/t3-file-icon-elixir.dark.svg"),
    ),
    (
        "t3-file-icon-haskell",
        Some("light"),
        include_bytes!("../../assets/pierre/t3-file-icon-haskell.light.svg"),
    ),
    (
        "t3-file-icon-haskell",
        Some("dark"),
        include_bytes!("../../assets/pierre/t3-file-icon-haskell.dark.svg"),
    ),
    (
        "t3-file-icon-java",
        Some("light"),
        include_bytes!("../../assets/pierre/t3-file-icon-java.light.svg"),
    ),
    (
        "t3-file-icon-java",
        Some("dark"),
        include_bytes!("../../assets/pierre/t3-file-icon-java.dark.svg"),
    ),
    (
        "t3-file-icon-kotlin",
        Some("light"),
        include_bytes!("../../assets/pierre/t3-file-icon-kotlin.light.svg"),
    ),
    (
        "t3-file-icon-kotlin",
        Some("dark"),
        include_bytes!("../../assets/pierre/t3-file-icon-kotlin.dark.svg"),
    ),
    (
        "t3-file-icon-lua",
        Some("light"),
        include_bytes!("../../assets/pierre/t3-file-icon-lua.light.svg"),
    ),
    (
        "t3-file-icon-lua",
        Some("dark"),
        include_bytes!("../../assets/pierre/t3-file-icon-lua.dark.svg"),
    ),
    (
        "t3-file-icon-php",
        Some("light"),
        include_bytes!("../../assets/pierre/t3-file-icon-php.light.svg"),
    ),
    (
        "t3-file-icon-php",
        Some("dark"),
        include_bytes!("../../assets/pierre/t3-file-icon-php.dark.svg"),
    ),
    (
        "t3-file-icon-powershell",
        Some("light"),
        include_bytes!("../../assets/pierre/t3-file-icon-powershell.light.svg"),
    ),
    (
        "t3-file-icon-powershell",
        Some("dark"),
        include_bytes!("../../assets/pierre/t3-file-icon-powershell.dark.svg"),
    ),
    (
        "t3-file-icon-r",
        Some("light"),
        include_bytes!("../../assets/pierre/t3-file-icon-r.light.svg"),
    ),
    (
        "t3-file-icon-r",
        Some("dark"),
        include_bytes!("../../assets/pierre/t3-file-icon-r.dark.svg"),
    ),
    (
        "t3-file-icon-scala",
        Some("light"),
        include_bytes!("../../assets/pierre/t3-file-icon-scala.light.svg"),
    ),
    (
        "t3-file-icon-scala",
        Some("dark"),
        include_bytes!("../../assets/pierre/t3-file-icon-scala.dark.svg"),
    ),
    (
        "t3-file-icon-toml",
        Some("light"),
        include_bytes!("../../assets/pierre/t3-file-icon-toml.light.svg"),
    ),
    (
        "t3-file-icon-toml",
        Some("dark"),
        include_bytes!("../../assets/pierre/t3-file-icon-toml.dark.svg"),
    ),
];

#[cfg(test)]
mod tests {
    use super::*;

    fn token(path: &str, mime: Option<&str>) -> Option<String> {
        resolve_pierre_icon_for_entry(path, false, mime).and_then(|icon| icon.token)
    }

    fn name(path: &str) -> Option<String> {
        resolve_pierre_icon_for_entry(path, false, None).map(|icon| icon.name)
    }

    #[test]
    fn uses_detected_types_for_extensionless_files_while_keeping_known_filename_icons() {
        assert_eq!(
            token("custom-script", Some("text/x-python")).as_deref(),
            Some("python")
        );
        assert_eq!(token("image", Some("image/png")).as_deref(), Some("image"));
        assert_eq!(
            token("Dockerfile", Some("text/x-shellscript")).as_deref(),
            Some("docker")
        );
        assert_eq!(
            resolve_pierre_icon_for_entry("folder.png", true, Some("image/png")),
            None
        );
        assert_eq!(
            token("unknown", Some("application/octet-stream")).as_deref(),
            Some("default")
        );
    }

    #[test]
    fn uses_pierre_exact_filename_and_complete_set_extension_mappings() {
        assert_eq!(token("Dockerfile", None).as_deref(), Some("docker"));
        assert_eq!(token("src/Button.tsx", None).as_deref(), Some("react"));
        assert_eq!(token("vite.config.ts", None).as_deref(), Some("vite"));
    }

    #[test]
    fn uses_built_in_pierre_icons_where_available() {
        assert_eq!(
            name("package.json").as_deref(),
            Some("file-tree-builtin-npm")
        );
        assert_eq!(
            name("config/tsconfig.json").as_deref(),
            Some("file-tree-builtin-typescript")
        );
        assert_eq!(
            name("CLAUDE.md").as_deref(),
            Some("file-tree-builtin-claude")
        );
        assert_eq!(
            name("README.md").as_deref(),
            Some("file-tree-builtin-markdown")
        );
    }

    #[test]
    fn extends_pierre_with_t3_specific_exact_filename_icons() {
        assert_eq!(name("AGENTS.md").as_deref(), Some("t3-file-icon-agents"));
        assert_eq!(name("pnpm-lock.yaml").as_deref(), Some("t3-file-icon-pnpm"));
        assert_eq!(
            name("pnpm-workspace.yaml").as_deref(),
            Some("t3-file-icon-pnpm")
        );
    }

    #[test]
    fn video_files_take_the_t3_video_icon() {
        for file in ["clip.mp4", "movie.MOV", "a/b/c.webm"] {
            assert_eq!(name(file).as_deref(), Some("t3-file-icon-video"), "{file}");
        }
    }

    #[test]
    fn ships_every_custom_icon_referenced_by_the_extended_resolver() {
        let referenced: std::collections::BTreeSet<&str> = T3_BY_FILE_NAME
            .iter()
            .chain(T3_BY_FILE_EXTENSION)
            .map(|(_, icon)| *icon)
            .chain([T3_VIDEO_ICON])
            .filter(|icon| icon.starts_with("t3-"))
            .collect();
        for icon in referenced {
            assert!(
                ICON_FILES.iter().any(|(id, _, _)| *id == icon),
                "missing asset for {icon}"
            );
            assert!(pierre_icon_asset(icon, false).is_some(), "{icon} light");
            assert!(pierre_icon_asset(icon, true).is_some(), "{icon} dark");
        }
    }

    #[test]
    fn ships_every_built_in_icon_the_tables_name() {
        let tokens: std::collections::BTreeSet<&str> = BUILT_IN_FILE_NAME_TOKENS
            .iter()
            .chain(BUILT_IN_FILE_EXTENSION_TOKENS)
            .chain(COMPLETE_EXTENSION_OVERRIDES)
            .map(|(_, token)| *token)
            .chain(["default"])
            .collect();
        for token in tokens {
            assert!(
                pierre_icon_asset(&built_in_file_icon_name(token), false).is_some(),
                "missing asset for {token}"
            );
        }
    }

    #[test]
    fn every_shipped_file_loads_by_its_asset_path() {
        for path in pierre_icon_asset_paths() {
            let bytes = load_pierre_icon(&path).unwrap_or_else(|| panic!("{path} not served"));
            assert!(bytes.starts_with(b"<svg"), "{path}");
        }
        assert!(load_pierre_icon("pierre/nope.svg").is_none());
        assert!(load_pierre_icon("brand/openai.svg").is_none());
    }

    #[test]
    fn light_dark_symbols_resolve_per_scheme() {
        assert_eq!(
            pierre_icon_asset_path("t3-file-icon-toml", false).as_deref(),
            Some("pierre/t3-file-icon-toml.light.svg")
        );
        assert_eq!(
            pierre_icon_asset_path("t3-file-icon-toml", true).as_deref(),
            Some("pierre/t3-file-icon-toml.dark.svg")
        );
        assert_eq!(
            pierre_icon_asset_path("t3-file-icon-pnpm", true).as_deref(),
            Some("pierre/t3-file-icon-pnpm.svg")
        );
    }

    #[test]
    fn uses_the_pierre_default_icon_for_unknown_file_types() {
        assert_eq!(
            token("artifact.unknown-ext", None).as_deref(),
            Some("default")
        );
        assert!(!has_specific_pierre_icon_for_file_name(
            "artifact.unknown-ext"
        ));
    }

    #[test]
    fn leaves_directory_rendering_to_the_shared_folder_fallback() {
        assert_eq!(
            resolve_pierre_icon_for_entry("packages/client-runtime", true, None),
            None
        );
    }

    #[test]
    fn normalizes_common_markdown_fence_language_aliases() {
        assert_eq!(synthetic_file_name_for_language_id("typescript"), "file.ts");
        assert_eq!(
            synthetic_file_name_for_language_id("dockerfile"),
            "Dockerfile"
        );
        assert_eq!(
            synthetic_file_name_for_language_id("shellscript"),
            "file.sh"
        );
        assert_eq!(synthetic_file_name_for_language_id("python"), "file.py");
        assert_eq!(
            synthetic_file_name_for_language_id("Unknown-Lang"),
            "file.unknown-lang"
        );
    }

    #[test]
    fn gives_a_dockerfile_fence_the_docker_icon() {
        assert!(has_specific_pierre_icon_for_file_name(
            &synthetic_file_name_for_language_id("dockerfile")
        ));
    }

    #[test]
    fn gives_a_language_fence_a_language_icon() {
        for language in [
            "csharp",
            "dart",
            "diff",
            "elixir",
            "haskell",
            "java",
            "kotlin",
            "lua",
            "php",
            "powershell",
            "r",
            "scala",
            "toml",
            "xml",
        ] {
            assert!(
                has_specific_pierre_icon_for_file_name(&synthetic_file_name_for_language_id(
                    language
                )),
                "{language}"
            );
        }
    }

    #[test]
    fn extension_candidates_run_from_the_longest_suffix() {
        assert_eq!(extension_candidates("a.b.c"), ["b.c", "c"]);
        assert_eq!(extension_candidates("Makefile"), Vec::<String>::new());
        // `.env.local` resolves through the two-part extension.
        assert_eq!(token("x.env.local", None).as_deref(), Some("text"));
        assert_eq!(basename_of_path("a/b/c.ts"), "c.ts");
    }

    #[test]
    fn token_colors_follow_the_pierre_palette() {
        assert_eq!(pierre_icon_token_color("python", false), Some(0x1a85d4));
        assert_eq!(pierre_icon_token_color("python", true), Some(0x69b1ff));
        assert_eq!(pierre_icon_token_color("git", false), Some(0xff8c5b));
        assert_eq!(pierre_icon_token_color("default", true), Some(0xadadb1));
        // Not tinted: Pierre leaves these on the muted foreground.
        assert_eq!(pierre_icon_token_color("font", false), None);
    }

    #[test]
    fn the_minimal_and_none_sets_resolve_no_token() {
        let candidates = extension_candidates("a.ts");
        assert_eq!(
            resolve_built_in_file_icon_token(BuiltInIconSet::Minimal, "a.ts", &candidates),
            None
        );
        assert_eq!(
            resolve_built_in_file_icon_token(BuiltInIconSet::None, "a.ts", &candidates),
            None
        );
        // The standard tier does not ship `react`; it falls back to the extension's own token.
        assert_eq!(
            resolve_built_in_file_icon_token(
                BuiltInIconSet::Standard,
                "a.tsx",
                &extension_candidates("a.tsx")
            ),
            Some("typescript")
        );
    }
}
