//! Deterministic name conversions between the canonical identifiers and each facade.
//!
//! Source ids match `^[a-z][a-z0-9-]*$` and operation names match `^[a-z][a-z0-9_]*$`, so an
//! MCP tool name `source_operation` always splits at its first underscore.

/// `github`, `my-notion`.
pub fn is_valid_source_id(id: &str) -> bool {
    let mut chars = id.chars();
    matches!(chars.next(), Some('a'..='z'))
        && chars.all(|c| matches!(c, 'a'..='z' | '0'..='9' | '-'))
}

/// `list_issues`.
pub fn is_valid_operation_name(name: &str) -> bool {
    let mut chars = name.chars();
    matches!(chars.next(), Some('a'..='z'))
        && chars.all(|c| matches!(c, 'a'..='z' | '0'..='9' | '_'))
}

/// `list_issues` -> `list-issues` (CLI subcommands and flags).
pub fn kebab(name: &str) -> String {
    name.replace('_', "-")
}

/// `list-issues` -> `list_issues`.
pub fn snake(name: &str) -> String {
    name.replace('-', "_")
}

/// `list_issues` -> `listIssues`, `my-notion` -> `myNotion` (GraphQL fields).
pub fn camel(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    let mut upper_next = false;
    for c in name.chars() {
        if c == '_' || c == '-' {
            upper_next = true;
        } else if upper_next {
            out.extend(c.to_uppercase());
            upper_next = false;
        } else {
            out.push(c);
        }
    }
    out
}

/// `list_issues` -> `ListIssues` (GraphQL type names).
pub fn pascal(name: &str) -> String {
    let camel = camel(name);
    let mut chars = camel.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}

/// `github`, `list_issues` -> `github_list_issues`.
pub fn mcp_tool_name(source: &str, operation: &str) -> String {
    format!("{source}_{operation}")
}

/// `github_list_issues` -> (`github`, `list_issues`).
pub fn split_mcp_tool_name(name: &str) -> Option<(&str, &str)> {
    let (source, operation) = name.split_once('_')?;
    (is_valid_source_id(source) && is_valid_operation_name(operation))
        .then_some((source, operation))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validates_identifiers() {
        assert!(is_valid_source_id("github"));
        assert!(is_valid_source_id("my-notion2"));
        assert!(!is_valid_source_id("my_notion"));
        assert!(!is_valid_source_id("Github"));
        assert!(!is_valid_source_id(""));
        assert!(is_valid_operation_name("list_issues"));
        assert!(!is_valid_operation_name("list-issues"));
        assert!(!is_valid_operation_name("_x"));
    }

    #[test]
    fn converts_cases() {
        assert_eq!(kebab("list_issues"), "list-issues");
        assert_eq!(snake("list-issues"), "list_issues");
        assert_eq!(camel("list_issues"), "listIssues");
        assert_eq!(camel("my-notion"), "myNotion");
        assert_eq!(pascal("list_issues"), "ListIssues");
    }

    #[test]
    fn mcp_names_round_trip() {
        let name = mcp_tool_name("my-notion", "search_pages");
        assert_eq!(name, "my-notion_search_pages");
        assert_eq!(
            split_mcp_tool_name(&name),
            Some(("my-notion", "search_pages"))
        );
        assert_eq!(split_mcp_tool_name("nounderscore"), None);
    }
}
