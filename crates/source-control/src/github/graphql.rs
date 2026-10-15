//! Port of upstream/packages/source-control-github/src/server/githubGraphQl.ts

use indexmap::IndexMap;
use serde_json::{Map, Value};

/// One GraphQL variable: the type it is declared as and the value it carries.
#[derive(Debug, Clone, PartialEq)]
pub struct GraphQlVariable {
    pub r#type: String,
    pub value: Value,
}

impl GraphQlVariable {
    pub fn new(r#type: impl Into<String>, value: impl Into<Value>) -> Self {
        Self {
            r#type: r#type.into(),
            value: value.into(),
        }
    }
}

/// GraphQL variables by name, in declaration order.
pub type GraphQlVariables = Vec<(String, GraphQlVariable)>;

/// The placeholders (`$s0_owner`) a `field` callback receives, by the item variable's name.
pub type GraphQlPlaceholders = IndexMap<String, String>;

#[derive(Debug, Clone, PartialEq)]
pub struct GraphQlDocument {
    pub query: String,
    pub variables: Map<String, Value>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GraphQlOperation {
    Query,
    Mutation,
}

impl GraphQlOperation {
    fn as_str(self) -> &'static str {
        match self {
            Self::Query => "query",
            Self::Mutation => "mutation",
        }
    }
}

/// `key` of [`AliasedGraphQlDocumentInput`]: the number an item's alias carries.
pub type GraphQlAliasKey<'a, Item> = &'a dyn Fn(&Item, usize) -> i64;

/// `until` of [`ReadGraphQlPagesOptions`].
pub type GraphQlUntil<'a, Page> = &'a dyn Fn(&[Page]) -> bool;

/// Input of [`aliased_graphql_document`].
pub struct AliasedGraphQlDocumentInput<'a, Item> {
    pub operation: GraphQlOperation,
    /// The operation's name, which tells one batch from another in logs and recorded traffic.
    pub name: Option<&'a str>,
    pub alias: &'a str,
    pub key: Option<GraphQlAliasKey<'a, Item>>,
    pub items: &'a [Item],
    pub variables: &'a dyn Fn(&Item) -> GraphQlVariables,
    pub field: &'a dyn Fn(&GraphQlPlaceholders, &Item) -> String,
    pub shared: GraphQlVariables,
    pub within: Option<&'a dyn Fn(&str) -> String>,
}

/// One document asking the same field for many items, each under its own alias: the prefix and
/// the item's position (`s0`, `s1`, ...), or its `key` when the answer is easier read by one.
/// GitHub has no bulk form of most lookups, so aliases are how a batch travels in one request.
///
/// Every value travels as a variable renamed per alias (`$s0_owner`), and `field` is handed those
/// placeholders rather than the values: nothing an item holds is written into the document.
/// `shared` variables are declared once and referenced by their own name; `within` nests the
/// aliased fields inside one parent selection, such as a single repository.
///
/// None for no items, since a document with no selection is not a document.
pub fn aliased_graphql_document<Item>(
    input: AliasedGraphQlDocumentInput<'_, Item>,
) -> Option<GraphQlDocument> {
    if input.items.is_empty() {
        return None;
    }
    let mut declarations: Vec<String> = Vec::new();
    let mut variables: Map<String, Value> = Map::new();
    for (name, variable) in &input.shared {
        declarations.push(format!("${name}: {}", variable.r#type));
        variables.insert(name.clone(), variable.value.clone());
    }
    let fields: Vec<String> = input
        .items
        .iter()
        .enumerate()
        .map(|(index, item)| {
            let key = input.key.map_or(index as i64, |key| key(item, index));
            let alias = format!("{}{key}", input.alias);
            let mut placeholders = GraphQlPlaceholders::new();
            for (name, variable) in (input.variables)(item) {
                let variable_name = format!("{alias}_{name}");
                declarations.push(format!("${variable_name}: {}", variable.r#type));
                variables.insert(variable_name.clone(), variable.value);
                placeholders.insert(name, format!("${variable_name}"));
            }
            format!("  {alias}: {}", (input.field)(&placeholders, item))
        })
        .collect();
    let selection = fields.join("\n");
    let parameters = if declarations.is_empty() {
        String::new()
    } else {
        format!("({})", declarations.join(", "))
    };
    let name = input.name.map_or(String::new(), |name| format!(" {name}"));
    let body = input
        .within
        .map_or_else(|| selection.clone(), |within| within(&selection));
    Some(GraphQlDocument {
        query: format!(
            "{}{name}{parameters} {{\n{body}\n}}",
            input.operation.as_str()
        ),
        variables,
    })
}

/// Options of [`read_graphql_pages`].
pub struct ReadGraphQlPagesOptions<'a, Page> {
    pub next_cursor: &'a dyn Fn(&Page) -> Option<String>,
    pub from: Option<String>,
    pub max_pages: Option<usize>,
    pub until: Option<GraphQlUntil<'a, Page>>,
}

/// What [`read_graphql_pages`] read. `truncated` says pages remained when reading stopped.
#[derive(Debug, Clone, PartialEq)]
pub struct GraphQlPages<Page> {
    pub pages: Vec<Page>,
    pub truncated: bool,
}

/// The pages of a GraphQL connection, read one after another from `from` (the first page by
/// default) until GitHub has no more, `until` is satisfied, or `max_pages` have been read. `read`
/// is handed the pages so far, for reads that size or check the next page by what has already
/// arrived.
///
/// `truncated` says pages remained when reading stopped. A cursor GitHub hands back a second time
/// would page forever, so it stops the read as truncated too.
pub async fn read_graphql_pages<Page, E>(
    mut read: impl AsyncFnMut(Option<String>, &[Page]) -> Result<Page, E>,
    options: ReadGraphQlPagesOptions<'_, Page>,
) -> Result<GraphQlPages<Page>, E> {
    let mut pages: Vec<Page> = Vec::new();
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut after = options.from;
    loop {
        let page = read(after.clone(), &pages).await?;
        let next = (options.next_cursor)(&page);
        pages.push(page);
        let Some(next) = next else {
            return Ok(GraphQlPages {
                pages,
                truncated: false,
            });
        };
        if seen.contains(&next)
            || pages.len() >= options.max_pages.unwrap_or(usize::MAX)
            || options.until.is_some_and(|until| until(&pages))
        {
            return Ok(GraphQlPages {
                pages,
                truncated: true,
            });
        }
        seen.insert(next.clone());
        after = Some(next);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn string_var(value: &str) -> GraphQlVariable {
        GraphQlVariable::new("String!", value)
    }

    #[test]
    fn keeps_every_value_out_of_the_document_each_under_its_aliass_own_variable() {
        let items = ["acme\") { x } #".to_owned(), "web".to_owned()];
        let document = aliased_graphql_document(AliasedGraphQlDocumentInput {
            operation: GraphQlOperation::Query,
            name: Some("Lookups"),
            alias: "s",
            key: None,
            items: &items,
            shared: vec![("owner".into(), string_var("acme"))],
            variables: &|repo: &String| vec![("name".into(), string_var(repo))],
            field: &|placeholders, _| {
                format!(
                    "repository(owner: $owner, name: {}) {{ id }}",
                    placeholders["name"]
                )
            },
            within: None,
        })
        .expect("a document");
        assert_eq!(
            document.query,
            "query Lookups($owner: String!, $s0_name: String!, $s1_name: String!) {\n\
             \x20 s0: repository(owner: $owner, name: $s0_name) { id }\n\
             \x20 s1: repository(owner: $owner, name: $s1_name) { id }\n}"
        );
        assert_eq!(
            Value::Object(document.variables),
            json!({ "owner": "acme", "s0_name": "acme\") { x } #", "s1_name": "web" })
        );
    }

    #[test]
    fn aliases_by_key_and_nests_inside_a_shared_parent_when_asked() {
        let document = aliased_graphql_document(AliasedGraphQlDocumentInput {
            operation: GraphQlOperation::Query,
            name: None,
            alias: "pr",
            key: Some(&|number: &i64, _| *number),
            items: &[2_i64, 3],
            shared: vec![],
            variables: &|number: &i64| {
                vec![("number".into(), GraphQlVariable::new("Int!", *number))]
            },
            field: &|placeholders, _| {
                format!("pullRequest(number: {}) {{ id }}", placeholders["number"])
            },
            within: Some(&|fields| {
                format!("repository(owner: \"a\", name: \"b\") {{\n{fields}\n}}")
            }),
        })
        .expect("a document");
        assert!(
            document
                .query
                .contains("  pr2: pullRequest(number: $pr2_number) { id }")
        );
        assert!(
            document
                .query
                .contains("  pr3: pullRequest(number: $pr3_number) { id }")
        );
        assert_eq!(
            Value::Object(document.variables),
            json!({ "pr2_number": 2, "pr3_number": 3 })
        );
        assert_eq!(
            document.query,
            "query($pr2_number: Int!, $pr3_number: Int!) {\nrepository(owner: \"a\", name: \"b\") {\n  pr2: pullRequest(number: $pr2_number) { id }\n  pr3: pullRequest(number: $pr3_number) { id }\n}\n}"
        );
    }

    #[test]
    fn asks_for_nothing_when_there_is_nothing_to_ask() {
        let document = aliased_graphql_document(AliasedGraphQlDocumentInput::<()> {
            operation: GraphQlOperation::Mutation,
            name: None,
            alias: "f",
            key: None,
            items: &[],
            shared: vec![],
            variables: &|_| vec![],
            field: &|_, _| "x".into(),
            within: None,
        });
        assert_eq!(document, None);
    }

    #[test]
    fn a_document_without_variables_has_no_parameter_list() {
        let document = aliased_graphql_document(AliasedGraphQlDocumentInput {
            operation: GraphQlOperation::Mutation,
            name: Some("M"),
            alias: "f",
            key: None,
            items: &[()],
            shared: vec![],
            variables: &|_| vec![],
            field: &|_, _| "x".into(),
            within: None,
        })
        .unwrap();
        assert_eq!(document.query, "mutation M {\n  f0: x\n}");
    }

    struct Pages {
        asked: std::cell::RefCell<Vec<Option<String>>>,
        cursors: Vec<Option<&'static str>>,
    }

    impl Pages {
        fn new(cursors: &[Option<&'static str>]) -> Self {
            Self {
                asked: Default::default(),
                cursors: cursors.to_vec(),
            }
        }
        async fn read(&self, after: Option<String>) -> Result<Option<String>, ()> {
            let mut asked = self.asked.borrow_mut();
            asked.push(after);
            Ok(self
                .cursors
                .get(asked.len() - 1)
                .copied()
                .flatten()
                .map(str::to_owned))
        }
        fn asked(&self) -> Vec<Option<String>> {
            self.asked.borrow().clone()
        }
    }

    fn options<'a>(
        next_cursor: &'a dyn Fn(&Option<String>) -> Option<String>,
    ) -> ReadGraphQlPagesOptions<'a, Option<String>> {
        ReadGraphQlPagesOptions {
            next_cursor,
            from: None,
            max_pages: None,
            until: None,
        }
    }

    #[tokio::test]
    async fn follows_the_cursor_until_github_has_no_more() {
        let pages = Pages::new(&[Some("a"), Some("b"), None]);
        let result = read_graphql_pages(
            async |after, _| pages.read(after).await,
            options(&|page| page.clone()),
        )
        .await
        .unwrap();
        assert_eq!(
            pages.asked(),
            vec![None, Some("a".to_owned()), Some("b".to_owned())]
        );
        assert!(!result.truncated);
        assert_eq!(result.pages.len(), 3);
    }

    #[tokio::test]
    async fn stops_as_truncated_at_the_page_limit_at_until_or_on_a_repeated_cursor() {
        let limited = Pages::new(&[Some("a"), Some("b"), Some("c")]);
        let at_limit = read_graphql_pages(
            async |after, _| limited.read(after).await,
            ReadGraphQlPagesOptions {
                max_pages: Some(2),
                ..options(&|page| page.clone())
            },
        )
        .await
        .unwrap();
        assert_eq!(limited.asked(), vec![None, Some("a".to_owned())]);
        assert!(at_limit.truncated);

        let enough = Pages::new(&[Some("a"), Some("b"), Some("c")]);
        let satisfied = read_graphql_pages(
            async |after, _| enough.read(after).await,
            ReadGraphQlPagesOptions {
                until: Some(&|read| read.len() == 1),
                ..options(&|page| page.clone())
            },
        )
        .await
        .unwrap();
        assert_eq!(enough.asked(), vec![None]);
        assert!(satisfied.truncated);

        let stuck = Pages::new(&[Some("a"), Some("a"), Some("a")]);
        let repeated = read_graphql_pages(
            async |after, _| stuck.read(after).await,
            ReadGraphQlPagesOptions {
                from: Some("start".into()),
                ..options(&|page| page.clone())
            },
        )
        .await
        .unwrap();
        assert_eq!(
            stuck.asked(),
            vec![Some("start".to_owned()), Some("a".to_owned())]
        );
        assert!(repeated.truncated);
    }

    #[tokio::test]
    async fn read_is_handed_the_pages_so_far_and_errors_propagate() {
        let mut seen_lengths = Vec::new();
        let result: Result<GraphQlPages<Option<String>>, &str> = read_graphql_pages(
            async |after, so_far: &[Option<String>]| {
                seen_lengths.push(so_far.len());
                match after.as_deref() {
                    None => Ok(Some("a".to_owned())),
                    Some("a") => Ok(Some("b".to_owned())),
                    Some(_) => Err("boom"),
                }
            },
            options(&|page| page.clone()),
        )
        .await;
        assert_eq!(result.unwrap_err(), "boom");
        assert_eq!(seen_lengths, vec![0, 1, 2]);
    }
}
