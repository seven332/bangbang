use std::fmt;

use serde::de::{self, MapAccess, SeqAccess, Visitor};
use serde::{Deserialize, Deserializer};

use crate::{CompileError, MAX_CONDITIONS_PER_RULE, MAX_JSON_BYTES, MAX_RULES_PER_THREAD};

const DUPLICATE_KEY_MARKER: &str = "bangbang duplicate JSON object key";
const REQUIRED_CATEGORIES: [&str; 3] = ["vmm", "api", "vcpu"];

pub(crate) struct Policy {
    filters: [(&'static str, Filter); 3],
}

impl Policy {
    pub(crate) fn into_filters(self) -> [(&'static str, Filter); 3] {
        self.filters
    }
}

pub(crate) struct Filter {
    pub(crate) default_action: Action,
    pub(crate) filter_action: Action,
    pub(crate) rules: Vec<Rule>,
}

pub(crate) struct Rule {
    pub(crate) syscall: String,
    pub(crate) conditions: Option<Vec<Condition>>,
}

pub(crate) struct Condition {
    pub(crate) index: u8,
    pub(crate) operator: CompareOperator,
    pub(crate) value: u64,
    pub(crate) value_length: ArgumentLength,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum CompareOperator {
    Eq,
    Ge,
    Gt,
    Le,
    Lt,
    MaskedEq(u64),
    Ne,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum ArgumentLength {
    Dword,
    Qword,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Action {
    Allow,
    Errno(u16),
    KillThread,
    KillProcess,
    Log,
    Trace(u16),
    Trap,
}

impl Action {
    pub(crate) const fn encode(self) -> u32 {
        match self {
            Self::Allow => 0x7fff_0000,
            Self::Errno(value) => 0x0005_0000 | value as u32,
            Self::KillThread => 0x0000_0000,
            Self::KillProcess => 0x8000_0000,
            Self::Log => 0x7ffc_0000,
            Self::Trace(value) => 0x7ff0_0000 | value as u32,
            Self::Trap => 0x0003_0000,
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PolicyWire {
    vmm: FilterWire,
    api: FilterWire,
    vcpu: FilterWire,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct FilterWire {
    default_action: ActionWire,
    filter_action: ActionWire,
    filter: Vec<RuleWire>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RuleWire {
    syscall: String,
    args: Option<Vec<ConditionWire>>,
    comment: Option<Comment>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ConditionWire {
    index: u8,
    op: CompareOperatorWire,
    val: u64,
    #[serde(rename = "type")]
    value_length: ArgumentLengthWire,
    comment: Option<Comment>,
}

#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
enum CompareOperatorWire {
    Eq,
    Ge,
    Gt,
    Le,
    Lt,
    MaskedEq(u64),
    Ne,
}

#[derive(Deserialize)]
#[serde(rename_all = "lowercase")]
enum ArgumentLengthWire {
    Dword,
    Qword,
}

#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
enum ActionWire {
    Allow,
    Errno(u16),
    KillThread,
    KillProcess,
    Log,
    Trace(u16),
    Trap,
}

struct Comment;

impl<'de> Deserialize<'de> for Comment {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_string(CommentVisitor)
    }
}

struct CommentVisitor;

impl Visitor<'_> for CommentVisitor {
    type Value = Comment;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("a string comment")
    }

    fn visit_str<E>(self, _value: &str) -> Result<Self::Value, E> {
        Ok(Comment)
    }

    fn visit_string<E>(self, _value: String) -> Result<Self::Value, E> {
        Ok(Comment)
    }
}

struct StrictJson(serde_json::Value);

impl<'de> Deserialize<'de> for StrictJson {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_any(StrictJsonVisitor).map(Self)
    }
}

struct StrictJsonVisitor;

impl<'de> Visitor<'de> for StrictJsonVisitor {
    type Value = serde_json::Value;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("a JSON value without duplicate object keys")
    }

    fn visit_bool<E>(self, value: bool) -> Result<Self::Value, E> {
        Ok(serde_json::Value::Bool(value))
    }

    fn visit_i64<E>(self, value: i64) -> Result<Self::Value, E> {
        Ok(serde_json::Value::Number(value.into()))
    }

    fn visit_u64<E>(self, value: u64) -> Result<Self::Value, E> {
        Ok(serde_json::Value::Number(value.into()))
    }

    fn visit_f64<E>(self, value: f64) -> Result<Self::Value, E>
    where
        E: de::Error,
    {
        serde_json::Number::from_f64(value)
            .map(serde_json::Value::Number)
            .ok_or_else(|| E::custom("invalid JSON number"))
    }

    fn visit_str<E>(self, value: &str) -> Result<Self::Value, E> {
        Ok(serde_json::Value::String(value.to_owned()))
    }

    fn visit_string<E>(self, value: String) -> Result<Self::Value, E> {
        Ok(serde_json::Value::String(value))
    }

    fn visit_none<E>(self) -> Result<Self::Value, E> {
        Ok(serde_json::Value::Null)
    }

    fn visit_unit<E>(self) -> Result<Self::Value, E> {
        Ok(serde_json::Value::Null)
    }

    fn visit_some<D>(self, deserializer: D) -> Result<Self::Value, D::Error>
    where
        D: Deserializer<'de>,
    {
        StrictJson::deserialize(deserializer).map(|value| value.0)
    }

    fn visit_seq<A>(self, mut sequence: A) -> Result<Self::Value, A::Error>
    where
        A: SeqAccess<'de>,
    {
        let mut values = Vec::with_capacity(sequence.size_hint().unwrap_or(0));
        while let Some(StrictJson(value)) = sequence.next_element()? {
            values.push(value);
        }
        Ok(serde_json::Value::Array(values))
    }

    fn visit_map<A>(self, mut map: A) -> Result<Self::Value, A::Error>
    where
        A: MapAccess<'de>,
    {
        let mut object = serde_json::Map::new();
        while let Some(key) = map.next_key::<String>()? {
            if object.contains_key(&key) {
                return Err(de::Error::custom(DUPLICATE_KEY_MARKER));
            }
            let StrictJson(value) = map.next_value()?;
            object.insert(key, value);
        }
        Ok(serde_json::Value::Object(object))
    }
}

pub(crate) fn parse(input: &str) -> Result<Policy, CompileError> {
    if input.len() > MAX_JSON_BYTES {
        return Err(CompileError::InputTooLarge);
    }

    let StrictJson(value) = serde_json::from_str::<StrictJson>(input).map_err(|error| {
        if error.to_string().contains(DUPLICATE_KEY_MARKER) {
            CompileError::DuplicateObjectKey
        } else {
            CompileError::InvalidJson
        }
    })?;

    validate_categories(&value)?;
    let policy: PolicyWire =
        serde_json::from_value(value).map_err(|_error| CompileError::InvalidSchema)?;

    Ok(Policy {
        filters: [
            ("vmm", convert_filter(policy.vmm)?),
            ("api", convert_filter(policy.api)?),
            ("vcpu", convert_filter(policy.vcpu)?),
        ],
    })
}

fn validate_categories(value: &serde_json::Value) -> Result<(), CompileError> {
    let object = value
        .as_object()
        .ok_or(CompileError::InvalidThreadCategories)?;
    if object.len() != REQUIRED_CATEGORIES.len()
        || REQUIRED_CATEGORIES
            .iter()
            .any(|category| !object.contains_key(*category))
    {
        return Err(CompileError::InvalidThreadCategories);
    }
    Ok(())
}

fn convert_filter(filter: FilterWire) -> Result<Filter, CompileError> {
    if filter.filter.len() > MAX_RULES_PER_THREAD {
        return Err(CompileError::TooManyRules);
    }

    let default_action = filter.default_action.into();
    let filter_action = filter.filter_action.into();
    if !filter.filter.is_empty() && default_action == filter_action {
        return Err(CompileError::IdenticalActions);
    }

    let mut rules = Vec::with_capacity(filter.filter.len());
    for rule in filter.filter {
        if rule
            .args
            .as_ref()
            .is_some_and(|conditions| conditions.len() > MAX_CONDITIONS_PER_RULE)
        {
            return Err(CompileError::TooManyConditions);
        }

        let conditions = rule
            .args
            .map(|conditions| {
                conditions
                    .into_iter()
                    .map(convert_condition)
                    .collect::<Result<Vec<_>, _>>()
            })
            .transpose()?;
        let _comment = rule.comment;
        rules.push(Rule {
            syscall: rule.syscall,
            conditions,
        });
    }

    Ok(Filter {
        default_action,
        filter_action,
        rules,
    })
}

fn convert_condition(condition: ConditionWire) -> Result<Condition, CompileError> {
    if condition.index > 5 {
        return Err(CompileError::InvalidArgumentIndex);
    }
    let _comment = condition.comment;
    Ok(Condition {
        index: condition.index,
        operator: condition.op.into(),
        value: condition.val,
        value_length: condition.value_length.into(),
    })
}

impl From<ActionWire> for Action {
    fn from(action: ActionWire) -> Self {
        match action {
            ActionWire::Allow => Self::Allow,
            ActionWire::Errno(value) => Self::Errno(value),
            ActionWire::KillThread => Self::KillThread,
            ActionWire::KillProcess => Self::KillProcess,
            ActionWire::Log => Self::Log,
            ActionWire::Trace(value) => Self::Trace(value),
            ActionWire::Trap => Self::Trap,
        }
    }
}

impl From<CompareOperatorWire> for CompareOperator {
    fn from(operator: CompareOperatorWire) -> Self {
        match operator {
            CompareOperatorWire::Eq => Self::Eq,
            CompareOperatorWire::Ge => Self::Ge,
            CompareOperatorWire::Gt => Self::Gt,
            CompareOperatorWire::Le => Self::Le,
            CompareOperatorWire::Lt => Self::Lt,
            CompareOperatorWire::MaskedEq(mask) => Self::MaskedEq(mask),
            CompareOperatorWire::Ne => Self::Ne,
        }
    }
}

impl From<ArgumentLengthWire> for ArgumentLength {
    fn from(length: ArgumentLengthWire) -> Self {
        match length {
            ArgumentLengthWire::Dword => Self::Dword,
            ArgumentLengthWire::Qword => Self::Qword,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const EMPTY_POLICY: &str = r#"{
        "vmm":{"default_action":"allow","filter_action":"trap","filter":[]},
        "api":{"default_action":"allow","filter_action":"trap","filter":[]},
        "vcpu":{"default_action":"allow","filter_action":"trap","filter":[]}
    }"#;

    #[test]
    fn accepts_exact_empty_policy_and_string_comments() {
        assert!(parse(EMPTY_POLICY).is_ok());
        let with_comments = EMPTY_POLICY.replace(
            "\"filter\":[]",
            "\"filter\":[{\"syscall\":\"read\",\"comment\":\"private\",\"args\":[{\"index\":0,\"type\":\"dword\",\"op\":\"eq\",\"val\":0,\"comment\":\"private\"}]}]",
        );
        assert!(parse(&with_comments).is_ok());
    }

    #[test]
    fn rejects_duplicate_keys_at_every_depth() {
        let root = format!("{{\"vmm\":{{}},\"vmm\":{{}},{}", &EMPTY_POLICY[1..]);
        assert_eq!(parse(&root).err(), Some(CompileError::DuplicateObjectKey));

        let nested = EMPTY_POLICY.replace(
            "\"default_action\":\"allow\"",
            "\"default_action\":\"allow\",\"default_action\":\"trap\"",
        );
        assert_eq!(parse(&nested).err(), Some(CompileError::DuplicateObjectKey));
    }

    #[test]
    fn rejects_category_and_schema_shapes() {
        assert_eq!(
            parse("[]").err(),
            Some(CompileError::InvalidThreadCategories)
        );
        let missing = EMPTY_POLICY.replace("\"vcpu\"", "\"private\"");
        assert_eq!(
            parse(&missing).err(),
            Some(CompileError::InvalidThreadCategories)
        );
        let unknown_field = EMPTY_POLICY.replace("\"filter\":[]", "\"filter\":[],\"private\":1");
        assert_eq!(
            parse(&unknown_field).err(),
            Some(CompileError::InvalidSchema)
        );
        assert_eq!(parse("{").err(), Some(CompileError::InvalidJson));
    }

    #[test]
    fn validates_equal_actions_only_when_rules_are_present() {
        let empty_equal =
            EMPTY_POLICY.replace("\"filter_action\":\"trap\"", "\"filter_action\":\"allow\"");
        assert!(parse(&empty_equal).is_ok());

        let nonempty =
            empty_equal.replacen("\"filter\":[]", "\"filter\":[{\"syscall\":\"read\"}]", 1);
        assert_eq!(parse(&nonempty).err(), Some(CompileError::IdenticalActions));
    }

    #[test]
    fn errors_never_retain_policy_values() {
        let sensitive = "private-syscall-name";
        let policy = EMPTY_POLICY.replacen(
            "\"filter\":[]",
            &format!("\"filter\":[{{\"syscall\":\"{sensitive}\",\"private\":1}}]"),
            1,
        );
        let error = parse(&policy).err().expect("policy should fail");
        assert!(!error.to_string().contains(sensitive));
        assert!(!format!("{error:?}").contains(sensitive));
    }
}
