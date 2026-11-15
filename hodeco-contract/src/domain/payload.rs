//! Protocol-neutral payload expression trees.
//!
//! The gateway never decodes protocol payloads for filtering. Expressions are
//! built by clients from masked byte leaves; this module validates and evaluates
//! them so the server, demo, and UI share one implementation.

use crate::utils::Base64Bytes;
use serde::de::{self, Deserializer};
use serde::ser::{SerializeStruct, Serializer};
use serde::{Deserialize, Serialize};
use std::fmt;

/// Maximum nesting depth of a payload expression, counting the root as 1.
pub const MAX_PAYLOAD_DEPTH: usize = 8;
/// Maximum number of leaves in one expression.
pub const MAX_PAYLOAD_LEAVES: usize = 64;
/// Maximum raw bytes in one leaf.
pub const MAX_PAYLOAD_LEAF_BYTES: usize = 32 * 1024;
/// Maximum raw bytes plus mask bytes across a whole expression.
pub const MAX_PAYLOAD_TOTAL_BYTES: usize = 64 * 1024;

/// Where a leaf pattern is anchored in the payload.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PayloadOffset {
    /// Exact byte offset. `At(0)` is the default when the field is omitted.
    At(u32),
    /// Slide the pattern over the whole payload (`offset: null`).
    Anywhere,
}

impl Default for PayloadOffset {
    fn default() -> Self {
        Self::At(0)
    }
}

impl PayloadOffset {
    pub const fn at(offset: u32) -> Self {
        Self::At(offset)
    }

    pub const fn anywhere() -> Self {
        Self::Anywhere
    }

    pub const fn is_default(self) -> bool {
        matches!(self, Self::At(0))
    }
}

/// Boolean operator for a payload group.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum PayloadOp {
    #[default]
    And,
    Or,
}

/// One contiguous masked byte match.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PayloadLeaf {
    pub offset: PayloadOffset,
    pub bytes: Base64Bytes,
    /// Optional per-byte mask. Missing or empty means all `0xFF`.
    pub mask: Option<Base64Bytes>,
    pub neg: bool,
}

impl PayloadLeaf {
    pub fn new(offset: PayloadOffset, bytes: impl Into<Vec<u8>>) -> Self {
        Self {
            offset,
            bytes: Base64Bytes::from(bytes.into()),
            mask: None,
            neg: false,
        }
    }

    pub fn with_mask(
        offset: PayloadOffset,
        bytes: impl Into<Vec<u8>>,
        mask: impl Into<Vec<u8>>,
    ) -> Self {
        let mask = mask.into();
        Self {
            offset,
            bytes: Base64Bytes::from(bytes.into()),
            mask: (!mask.is_empty()).then(|| Base64Bytes::from(mask)),
            neg: false,
        }
    }

    pub fn negated(mut self) -> Self {
        self.neg = true;
        self
    }
}

/// A boolean group of payload expressions.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PayloadGroup {
    pub op: PayloadOp,
    pub neg: bool,
    pub args: Vec<PayloadExpr>,
}

/// A leaf or a boolean group. Deserialization rejects malformed nodes and
/// expressions that exceed the documented limits.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PayloadExpr {
    Leaf(PayloadLeaf),
    Group(PayloadGroup),
}

impl PayloadExpr {
    pub fn leaf(offset: PayloadOffset, bytes: impl Into<Vec<u8>>) -> Self {
        Self::Leaf(PayloadLeaf::new(offset, bytes))
    }

    pub fn masked_leaf(
        offset: PayloadOffset,
        bytes: impl Into<Vec<u8>>,
        mask: impl Into<Vec<u8>>,
    ) -> Self {
        Self::Leaf(PayloadLeaf::with_mask(offset, bytes, mask))
    }

    pub fn anywhere(bytes: impl Into<Vec<u8>>) -> Self {
        Self::leaf(PayloadOffset::anywhere(), bytes)
    }

    pub fn and(args: Vec<Self>) -> Self {
        Self::Group(PayloadGroup {
            op: PayloadOp::And,
            neg: false,
            args,
        })
    }

    pub fn or(args: Vec<Self>) -> Self {
        Self::Group(PayloadGroup {
            op: PayloadOp::Or,
            neg: false,
            args,
        })
    }

    pub fn negated(self) -> Self {
        match self {
            Self::Leaf(mut leaf) => {
                leaf.neg = true;
                Self::Leaf(leaf)
            }
            Self::Group(mut group) => {
                group.neg = true;
                Self::Group(group)
            }
        }
    }

    /// Evaluates the expression against a payload. Invalid empty leaves and
    /// groups evaluate to `false`; valid API requests are checked by
    /// [`Self::validate`] before they reach this method.
    pub fn matches(&self, payload: &[u8]) -> bool {
        match self {
            Self::Leaf(leaf) => {
                let bytes = leaf.bytes.as_slice();
                if bytes.is_empty() {
                    return false;
                }
                let mask = leaf
                    .mask
                    .as_ref()
                    .filter(|mask| !mask.as_slice().is_empty())
                    .map(Base64Bytes::as_slice);
                let length = mask.map_or(bytes.len(), |mask| mask.len().min(bytes.len()));
                if length == 0 {
                    return false;
                }
                let matches_at = |candidate: &[u8]| {
                    candidate.len() == length
                        && candidate.iter().zip(bytes).enumerate().take(length).all(
                            |(index, (actual, expected))| {
                                let mask = mask
                                    .and_then(|mask| mask.get(index).copied())
                                    .unwrap_or(0xff);
                                (actual & mask) == (expected & mask)
                            },
                        )
                };
                let matched = match leaf.offset {
                    PayloadOffset::At(offset) => usize::try_from(offset)
                        .ok()
                        .and_then(|offset| payload.get(offset..offset.saturating_add(length)))
                        .is_some_and(matches_at),
                    PayloadOffset::Anywhere => payload.windows(length).any(matches_at),
                };
                if leaf.neg { !matched } else { matched }
            }
            Self::Group(group) => {
                if group.args.is_empty() {
                    return false;
                }
                let result = match group.op {
                    PayloadOp::And => group.args.iter().all(|arg| arg.matches(payload)),
                    PayloadOp::Or => group.args.iter().any(|arg| arg.matches(payload)),
                };
                if group.neg { !result } else { result }
            }
        }
    }

    /// Checks shape and resource limits. Deserialization performs the same
    /// checks, so this is primarily for programmatically built expressions.
    pub fn validate(&self) -> Result<(), PayloadExprError> {
        let mut stats = Stats::default();
        self.validate_node(1, &mut stats)?;
        if stats.leaves > MAX_PAYLOAD_LEAVES {
            return Err(PayloadExprError::TooManyLeaves);
        }
        if stats.bytes > MAX_PAYLOAD_TOTAL_BYTES {
            return Err(PayloadExprError::TooManyBytes);
        }
        Ok(())
    }

    fn validate_node(&self, depth: usize, stats: &mut Stats) -> Result<(), PayloadExprError> {
        if depth > MAX_PAYLOAD_DEPTH {
            return Err(PayloadExprError::TooDeep);
        }
        match self {
            Self::Leaf(leaf) => {
                if leaf.bytes.as_slice().is_empty() {
                    return Err(PayloadExprError::EmptyLeaf);
                }
                if leaf.bytes.len() > MAX_PAYLOAD_LEAF_BYTES {
                    return Err(PayloadExprError::LeafTooLarge);
                }
                stats.leaves += 1;
                stats.bytes +=
                    leaf.bytes.len() + leaf.mask.as_ref().map_or(0, |mask| mask.as_slice().len());
            }
            Self::Group(group) => {
                if group.args.is_empty() {
                    return Err(PayloadExprError::EmptyGroup);
                }
                for arg in &group.args {
                    arg.validate_node(depth + 1, stats)?;
                }
            }
        }
        Ok(())
    }
}

#[derive(Debug, Default)]
struct Stats {
    leaves: usize,
    bytes: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PayloadExprError {
    #[error("payload expression is nested deeper than {MAX_PAYLOAD_DEPTH} levels")]
    TooDeep,
    #[error("payload expression has more than {MAX_PAYLOAD_LEAVES} leaves")]
    TooManyLeaves,
    #[error(
        "payload expression uses more than {MAX_PAYLOAD_TOTAL_BYTES} bytes of pattern and mask data"
    )]
    TooManyBytes,
    #[error("payload leaf is larger than {MAX_PAYLOAD_LEAF_BYTES} bytes")]
    LeafTooLarge,
    #[error("payload leaf bytes must not be empty")]
    EmptyLeaf,
    #[error("payload group must contain at least one argument")]
    EmptyGroup,
    #[error("payload node must be either a leaf (bytes) or a group (args)")]
    InvalidNode,
    #[error("a payload leaf cannot have op or args")]
    InvalidLeaf,
    #[error("a payload group cannot have offset, bytes, or mask")]
    InvalidGroup,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct RawNode {
    #[serde(default)]
    op: Option<PayloadOp>,
    #[serde(default)]
    neg: bool,
    #[serde(default, deserialize_with = "deserialize_double_option")]
    offset: Option<Option<u32>>,
    #[serde(default)]
    bytes: Option<Base64Bytes>,
    #[serde(default)]
    mask: Option<Base64Bytes>,
    #[serde(default)]
    args: Option<Vec<Self>>,
}

fn deserialize_double_option<'de, D>(deserializer: D) -> Result<Option<Option<u32>>, D::Error>
where
    D: Deserializer<'de>,
{
    Option::<u32>::deserialize(deserializer).map(Some)
}

impl RawNode {
    fn into_expr(self) -> Result<PayloadExpr, PayloadExprError> {
        match (self.bytes, self.args) {
            (Some(_), Some(_)) => Err(PayloadExprError::InvalidNode),
            (None, None) => Err(PayloadExprError::InvalidNode),
            (Some(bytes), None) => {
                if self.op.is_some() {
                    return Err(PayloadExprError::InvalidLeaf);
                }
                if bytes.as_slice().is_empty() {
                    return Err(PayloadExprError::EmptyLeaf);
                }
                let offset = match self.offset {
                    None => PayloadOffset::At(0),
                    Some(Some(offset)) => PayloadOffset::At(offset),
                    Some(None) => PayloadOffset::Anywhere,
                };
                Ok(PayloadExpr::Leaf(PayloadLeaf {
                    offset,
                    bytes,
                    mask: self.mask.filter(|mask| !mask.as_slice().is_empty()),
                    neg: self.neg,
                }))
            }
            (None, Some(args)) => {
                if self.offset.is_some() || self.mask.is_some() {
                    return Err(PayloadExprError::InvalidGroup);
                }
                if args.is_empty() {
                    return Err(PayloadExprError::EmptyGroup);
                }
                let mut expressions = Vec::with_capacity(args.len());
                for arg in args {
                    expressions.push(arg.into_expr()?);
                }
                Ok(PayloadExpr::Group(PayloadGroup {
                    op: self.op.unwrap_or_default(),
                    neg: self.neg,
                    args: expressions,
                }))
            }
        }
    }
}

impl<'de> Deserialize<'de> for PayloadExpr {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let expression = RawNode::deserialize(deserializer)
            .and_then(|node| node.into_expr().map_err(de::Error::custom))?;
        expression.validate().map_err(de::Error::custom)?;
        Ok(expression)
    }
}

impl Serialize for PayloadExpr {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        match self {
            Self::Leaf(leaf) => {
                let mut state = serializer.serialize_struct(
                    "PayloadLeaf",
                    usize::from(!leaf.offset.is_default())
                        + 1
                        + usize::from(leaf.mask.is_some())
                        + usize::from(leaf.neg),
                )?;
                match leaf.offset {
                    PayloadOffset::At(0) => {}
                    PayloadOffset::At(offset) => state.serialize_field("offset", &offset)?,
                    PayloadOffset::Anywhere => {
                        state.serialize_field("offset", &None::<u32>)?;
                    }
                }
                state.serialize_field("bytes", &leaf.bytes)?;
                if let Some(mask) = &leaf.mask
                    && !mask.as_slice().is_empty()
                {
                    state.serialize_field("mask", mask)?;
                }
                if leaf.neg {
                    state.serialize_field("neg", &true)?;
                }
                state.end()
            }
            Self::Group(group) => {
                let mut state =
                    serializer.serialize_struct("PayloadGroup", 2 + usize::from(group.neg))?;
                state.serialize_field("op", &group.op)?;
                if group.neg {
                    state.serialize_field("neg", &true)?;
                }
                state.serialize_field("args", &group.args)?;
                state.end()
            }
        }
    }
}

impl fmt::Display for PayloadOffset {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::At(offset) => write!(formatter, "{offset}"),
            Self::Anywhere => formatter.write_str("anywhere"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn leaf(bytes: &[u8]) -> serde_json::Value {
        serde_json::json!({"bytes": Base64Bytes::from(bytes)})
    }

    #[test]
    fn omitted_offset_defaults_to_zero_and_null_is_anywhere() {
        let expression: PayloadExpr = serde_json::from_value(serde_json::json!({
            "bytes": "AQID"
        }))
        .unwrap();
        let PayloadExpr::Leaf(leaf) = expression else {
            panic!("expected leaf");
        };
        assert_eq!(leaf.offset, PayloadOffset::At(0));

        let expression: PayloadExpr = serde_json::from_value(serde_json::json!({
            "offset": null,
            "bytes": "AQID"
        }))
        .unwrap();
        let PayloadExpr::Leaf(leaf) = expression else {
            panic!("expected leaf");
        };
        assert_eq!(leaf.offset, PayloadOffset::Anywhere);

        let expression: PayloadExpr = serde_json::from_value(serde_json::json!({
            "offset": 3,
            "bytes": "AQID"
        }))
        .unwrap();
        let PayloadExpr::Leaf(leaf) = expression else {
            panic!("expected leaf");
        };
        assert_eq!(leaf.offset, PayloadOffset::At(3));
    }

    #[test]
    fn serializes_compact_defaults_and_null_offset() {
        assert_eq!(
            serde_json::to_value(PayloadExpr::leaf(PayloadOffset::At(0), vec![1, 2, 3])).unwrap(),
            serde_json::json!({"bytes": "AQID"})
        );
        assert_eq!(
            serde_json::to_value(PayloadExpr::anywhere(vec![1, 2, 3])).unwrap(),
            serde_json::json!({"offset": null, "bytes": "AQID"})
        );
        assert_eq!(
            serde_json::to_value(PayloadExpr::or(vec![
                PayloadExpr::leaf(PayloadOffset::At(4), vec![6]),
                PayloadExpr::leaf(PayloadOffset::At(4), vec![7]),
            ]))
            .unwrap(),
            serde_json::json!({
                "op": "or",
                "args": [{"offset": 4, "bytes": "Bg=="}, {"offset": 4, "bytes": "Bw=="}]
            })
        );
    }

    #[test]
    fn mask_normalization_truncates_and_pads_with_ff() {
        let short = PayloadExpr::masked_leaf(PayloadOffset::At(1), vec![1, 2, 3], vec![0xff]);
        assert!(short.matches(&[0, 1, 9, 9]));
        assert!(!short.matches(&[0, 2, 2, 3]));

        let long = PayloadExpr::masked_leaf(PayloadOffset::At(0), vec![1, 2], vec![0xff, 0, 0, 0]);
        assert!(long.matches(&[1, 9]));

        let wildcard =
            PayloadExpr::masked_leaf(PayloadOffset::At(0), vec![1, 2, 3], vec![0xff, 0, 0]);
        assert!(wildcard.matches(&[1, 9, 8]));
        assert!(!wildcard.matches(&[9, 2, 3]));
    }

    #[test]
    fn empty_mask_is_exact() {
        let expression: PayloadExpr = serde_json::from_value(serde_json::json!({
            "offset": 0,
            "bytes": "AQID",
            "mask": ""
        }))
        .unwrap();
        assert!(expression.matches(&[1, 2, 3]));
        assert!(!expression.matches(&[1, 2, 4]));
    }

    #[test]
    fn anywhere_matches_a_sliding_window() {
        let expression = PayloadExpr::anywhere(vec![2, 3, 4]);
        assert!(expression.matches(&[1, 2, 3, 4, 5]));
        assert!(!expression.matches(&[1, 2, 3]));
    }

    #[test]
    fn neg_applies_to_leaf_and_group_results() {
        let leaf = PayloadExpr::anywhere(vec![1]).negated();
        assert!(!leaf.matches(&[1]));
        assert!(leaf.matches(&[2]));

        let group = PayloadExpr::and(vec![
            PayloadExpr::anywhere(vec![1]),
            PayloadExpr::anywhere(vec![2]),
        ])
        .negated();
        assert!(!group.matches(&[1, 2]));
        assert!(group.matches(&[1]));
    }

    #[test]
    fn groups_default_to_and_and_support_or() {
        let and: PayloadExpr = serde_json::from_value(serde_json::json!({
            "args": [
                {"offset": 0, "bytes": "AQ=="},
                {"offset": 1, "bytes": "Ag=="}
            ]
        }))
        .unwrap();
        assert!(and.matches(&[1, 2]));
        assert!(!and.matches(&[1]));

        let or: PayloadExpr = serde_json::from_value(serde_json::json!({
            "op": "or",
            "args": [
                {"offset": 0, "bytes": "AQ=="},
                {"offset": 0, "bytes": "Ag=="}
            ]
        }))
        .unwrap();
        assert!(or.matches(&[2]));
        assert!(!or.matches(&[3]));
    }

    #[test]
    fn rejects_malformed_nodes() {
        for value in [
            serde_json::json!({}),
            serde_json::json!({"bytes": "", "mask": ""}),
            serde_json::json!({"bytes": "AQ==", "args": []}),
            serde_json::json!({"op": "or", "bytes": "AQ=="}),
            serde_json::json!({"op": "or", "args": []}),
            serde_json::json!({"op": "or", "args": [leaf(&[1])], "offset": 0}),
            serde_json::json!({"op": "or", "args": [leaf(&[1])], "mask": ""}),
            serde_json::json!({"args": [leaf(&[1])], "unknown": true}),
        ] {
            assert!(
                serde_json::from_value::<PayloadExpr>(value.clone()).is_err(),
                "accepted {value}"
            );
        }
    }

    #[test]
    fn enforces_depth_leaf_and_byte_limits() {
        let mut expression = PayloadExpr::leaf(PayloadOffset::At(0), vec![1]);
        for _ in 0..MAX_PAYLOAD_DEPTH {
            expression = PayloadExpr::and(vec![expression]);
        }
        assert_eq!(expression.validate(), Err(PayloadExprError::TooDeep));

        let leaves = (0..=MAX_PAYLOAD_LEAVES)
            .map(|_| PayloadExpr::leaf(PayloadOffset::At(0), vec![1]))
            .collect();
        assert_eq!(
            PayloadExpr::or(leaves).validate(),
            Err(PayloadExprError::TooManyLeaves)
        );

        let too_large =
            PayloadExpr::leaf(PayloadOffset::At(0), vec![0; MAX_PAYLOAD_LEAF_BYTES + 1]);
        assert_eq!(too_large.validate(), Err(PayloadExprError::LeafTooLarge));

        let many = (0..3)
            .map(|_| PayloadExpr::leaf(PayloadOffset::At(0), vec![0; MAX_PAYLOAD_LEAF_BYTES]))
            .collect();
        assert_eq!(
            PayloadExpr::or(many).validate(),
            Err(PayloadExprError::TooManyBytes)
        );
    }

    #[test]
    fn short_circuit_evaluation_matches_full_evaluation() {
        let expression = PayloadExpr::and(vec![
            PayloadExpr::anywhere(vec![0xff]).negated(),
            PayloadExpr::anywhere(vec![1]),
        ]);
        assert!(!expression.matches(&[2]));
        assert!(!expression.matches(&[0xff, 1]));
        assert!(expression.matches(&[1]));
    }
}
