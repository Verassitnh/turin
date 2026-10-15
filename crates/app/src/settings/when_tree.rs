//! The `when` expression tree edits of KeybindingsSettings.tsx
//! (`flattenWhenChildren`, `buildWhenExpressionGroup`, `conditionParts`,
//! `setCondition*`, `defaultWhen*`), as pure functions.

use turin_contracts::keybindings::KeybindingWhenNode as Node;

use super::keybindings_logic::default_when_variable;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BooleanOperator {
    And,
    Or,
}

impl BooleanOperator {
    pub fn label(self) -> &'static str {
        match self {
            Self::And => "and",
            Self::Or => "or",
        }
    }
}

fn matches_operator(node: &Node, operator: BooleanOperator) -> Option<(&Node, &Node)> {
    match (node, operator) {
        (Node::And { left, right }, BooleanOperator::And)
        | (Node::Or { left, right }, BooleanOperator::Or) => Some((left, right)),
        _ => None,
    }
}

/// `flattenWhenChildren`.
pub fn flatten_when_children(node: &Node, operator: BooleanOperator) -> Vec<Node> {
    match matches_operator(node, operator) {
        None => vec![node.clone()],
        Some((left, right)) => {
            let mut children = flatten_when_children(left, operator);
            children.extend(flatten_when_children(right, operator));
            children
        }
    }
}

/// `buildWhenExpressionGroup`: left-nested chain of `operator`.
pub fn build_when_expression_group(children: &[Node], operator: BooleanOperator) -> Option<Node> {
    let (first, rest) = children.split_first()?;
    Some(
        rest.iter()
            .fold(first.clone(), |left, right| match operator {
                BooleanOperator::And => Node::And {
                    left: Box::new(left),
                    right: Box::new(right.clone()),
                },
                BooleanOperator::Or => Node::Or {
                    left: Box::new(left),
                    right: Box::new(right.clone()),
                },
            }),
    )
}

/// `conditionParts`: a bare identifier or a negated one.
pub fn condition_parts(node: &Node) -> Option<(String, bool)> {
    match node {
        Node::Identifier { name } => Some((name.clone(), false)),
        Node::Not { node } => match &**node {
            Node::Identifier { name } => Some((name.clone(), true)),
            _ => None,
        },
        _ => None,
    }
}

pub fn set_condition_identifier(node: &Node, identifier: &str) -> Node {
    let Some((_, negated)) = condition_parts(node) else {
        return node.clone();
    };
    let next = Node::Identifier {
        name: identifier.to_owned(),
    };
    if negated {
        Node::Not {
            node: Box::new(next),
        }
    } else {
        next
    }
}

pub fn set_condition_negated(node: &Node, negated: bool) -> Node {
    match condition_parts(node) {
        None => {
            if negated {
                Node::Not {
                    node: Box::new(node.clone()),
                }
            } else {
                node.clone()
            }
        }
        Some((identifier, _)) => {
            let identifier = Node::Identifier { name: identifier };
            if negated {
                Node::Not {
                    node: Box::new(identifier),
                }
            } else {
                identifier
            }
        }
    }
}

pub fn default_when_condition() -> Node {
    Node::Identifier {
        name: default_when_variable(),
    }
}

pub fn default_when_group(operator: BooleanOperator) -> Node {
    let left = Box::new(default_when_condition());
    let right = Box::new(Node::Not {
        node: Box::new(default_when_condition()),
    });
    match operator {
        BooleanOperator::And => Node::And { left, right },
        BooleanOperator::Or => Node::Or { left, right },
    }
}

/// The operator a group node uses (`node.type === "or" ? "or" : "and"`).
pub fn node_operator(node: &Node) -> BooleanOperator {
    if matches!(node, Node::Or { .. }) {
        BooleanOperator::Or
    } else {
        BooleanOperator::And
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ident(name: &str) -> Node {
        Node::Identifier { name: name.into() }
    }

    #[test]
    fn flattens_and_rebuilds_same_operator_chains() {
        let chain = build_when_expression_group(
            &[ident("a"), ident("b"), ident("c")],
            BooleanOperator::And,
        )
        .unwrap();
        assert_eq!(
            flatten_when_children(&chain, BooleanOperator::And),
            vec![ident("a"), ident("b"), ident("c")]
        );
        // A different operator is one child.
        assert_eq!(flatten_when_children(&chain, BooleanOperator::Or).len(), 1);
        assert_eq!(build_when_expression_group(&[], BooleanOperator::And), None);
    }

    #[test]
    fn negates_and_renames_conditions() {
        let negated = set_condition_negated(&ident("a"), true);
        assert_eq!(condition_parts(&negated), Some(("a".into(), true)));
        assert_eq!(set_condition_negated(&negated, false), ident("a"));
        let renamed = set_condition_identifier(&negated, "b");
        assert_eq!(condition_parts(&renamed), Some(("b".into(), true)));
        // Groups are wrapped, not edited.
        let group = default_when_group(BooleanOperator::And);
        assert_eq!(condition_parts(&group), None);
        assert!(matches!(
            set_condition_negated(&group, true),
            Node::Not { .. }
        ));
    }

    #[test]
    fn default_groups_use_the_first_default_when_variable() {
        let group = default_when_group(BooleanOperator::Or);
        let children = flatten_when_children(&group, BooleanOperator::Or);
        assert_eq!(children.len(), 2);
        assert_eq!(node_operator(&group), BooleanOperator::Or);
    }
}
