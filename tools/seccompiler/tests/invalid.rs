use bangbang_seccompiler::{
    CompileError, CompileOptions, MAX_CONDITIONS_PER_RULE, MAX_JSON_BYTES, MAX_RULES_PER_THREAD,
    TargetArch, compile_json,
};

fn policy(filter: &str) -> String {
    format!(r#"{{"vmm":{filter},"api":{filter},"vcpu":{filter}}}"#)
}

fn filter(default_action: &str, filter_action: &str, rules: &str) -> String {
    format!(
        r#"{{"default_action":{default_action},"filter_action":{filter_action},"filter":{rules}}}"#
    )
}

#[test]
fn rejects_public_schema_and_category_failures() {
    let empty = filter("\"allow\"", "\"trap\"", "[]");
    assert_eq!(
        compile_json("[]", TargetArch::X86_64, CompileOptions::new()).err(),
        Some(CompileError::InvalidThreadCategories)
    );
    assert_eq!(
        compile_json(
            &format!(r#"{{"vmm":{empty},"api":{empty}}}"#),
            TargetArch::X86_64,
            CompileOptions::new(),
        )
        .err(),
        Some(CompileError::InvalidThreadCategories)
    );
    assert_eq!(
        compile_json(
            &format!(r#"{{"vmm":{empty},"api":{empty},"vcpu":{empty},"private":{empty}}}"#),
            TargetArch::X86_64,
            CompileOptions::new(),
        )
        .err(),
        Some(CompileError::InvalidThreadCategories)
    );

    for rules in [
        r#"[{"syscall":"read","private":1}]"#,
        r#"[{"syscall":"read","args":[{"index":0,"type":"private","op":"eq","val":0}]}]"#,
        r#"[{"syscall":"read","args":[{"index":0,"type":"qword","op":"private","val":0}]}]"#,
        r#"[{"syscall":"read","args":[{"index":0,"type":"qword","op":"eq","val":0,"comment":1}]}]"#,
        r#"[{"syscall":"read","args":[{"index":0,"type":"qword","op":"eq","val":0}],"comment":1}]"#,
    ] {
        let input = policy(&filter("\"trap\"", "\"allow\"", rules));
        assert_eq!(
            compile_json(&input, TargetArch::X86_64, CompileOptions::new()).err(),
            Some(CompileError::InvalidSchema)
        );
    }

    let oversized_action = policy(&filter(
        "\"trap\"",
        r#"{"errno":65536}"#,
        r#"[{"syscall":"read"}]"#,
    ));
    assert_eq!(
        compile_json(&oversized_action, TargetArch::X86_64, CompileOptions::new(),).err(),
        Some(CompileError::InvalidSchema)
    );
}

#[test]
fn rejects_duplicate_keys_and_invalid_json() {
    let input = r#"{
        "vmm":{"default_action":"trap","default_action":"allow","filter_action":"allow","filter":[]},
        "api":{"default_action":"trap","filter_action":"allow","filter":[]},
        "vcpu":{"default_action":"trap","filter_action":"allow","filter":[]}
    }"#;
    assert_eq!(
        compile_json(input, TargetArch::X86_64, CompileOptions::new()).err(),
        Some(CompileError::DuplicateObjectKey)
    );
    assert_eq!(
        compile_json("{", TargetArch::X86_64, CompileOptions::new()).err(),
        Some(CompileError::InvalidJson)
    );
}

#[test]
fn enforces_argument_and_condition_bounds() {
    let invalid_index = policy(&filter(
        "\"trap\"",
        "\"allow\"",
        r#"[{"syscall":"read","args":[{"index":6,"type":"qword","op":"eq","val":0}]}]"#,
    ));
    assert_eq!(
        compile_json(&invalid_index, TargetArch::X86_64, CompileOptions::new(),).err(),
        Some(CompileError::InvalidArgumentIndex)
    );

    let condition = r#"{"index":0,"type":"qword","op":"eq","val":0}"#;
    let conditions = std::iter::repeat_n(condition, MAX_CONDITIONS_PER_RULE + 1)
        .collect::<Vec<_>>()
        .join(",");
    let too_many = policy(&filter(
        "\"trap\"",
        "\"allow\"",
        &format!(r#"[{{"syscall":"read","args":[{conditions}]}}]"#),
    ));
    assert_eq!(
        compile_json(&too_many, TargetArch::X86_64, CompileOptions::new()).err(),
        Some(CompileError::TooManyConditions)
    );
}

#[test]
fn enforces_rule_and_input_bounds() {
    let rule = r#"{"syscall":"read"}"#;
    let rules = std::iter::repeat_n(rule, MAX_RULES_PER_THREAD + 1)
        .collect::<Vec<_>>()
        .join(",");
    let input = policy(&filter("\"trap\"", "\"allow\"", &format!("[{rules}]")));
    assert_eq!(
        compile_json(&input, TargetArch::X86_64, CompileOptions::new()).err(),
        Some(CompileError::TooManyRules)
    );

    let oversized = " ".repeat(MAX_JSON_BYTES + 1);
    assert_eq!(
        compile_json(&oversized, TargetArch::X86_64, CompileOptions::new()).err(),
        Some(CompileError::InputTooLarge)
    );
}

#[test]
fn rejects_unknown_syscalls_and_equal_actions_with_rules() {
    let sensitive = "private-syscall-name";
    let unknown = policy(&filter(
        "\"trap\"",
        "\"allow\"",
        &format!(r#"[{{"syscall":"{sensitive}"}}]"#),
    ));
    let error = compile_json(&unknown, TargetArch::X86_64, CompileOptions::new())
        .expect_err("unknown syscall should fail");
    assert_eq!(error, CompileError::UnknownSyscall);
    assert!(!error.to_string().contains(sensitive));
    assert!(!format!("{error:?}").contains(sensitive));

    let equal = policy(&filter("\"allow\"", "\"allow\"", r#"[{"syscall":"read"}]"#));
    assert_eq!(
        compile_json(&equal, TargetArch::X86_64, CompileOptions::new()).err(),
        Some(CompileError::IdenticalActions)
    );
}

#[test]
fn rejects_generated_program_over_kernel_limit() {
    let condition = r#"{"index":0,"type":"qword","op":"eq","val":0}"#;
    let conditions = std::iter::repeat_n(condition, MAX_CONDITIONS_PER_RULE)
        .collect::<Vec<_>>()
        .join(",");
    let rule = format!(r#"{{"syscall":"read","args":[{conditions}]}}"#);
    let rules = std::iter::repeat_n(rule.as_str(), MAX_RULES_PER_THREAD)
        .collect::<Vec<_>>()
        .join(",");
    let input = policy(&filter("\"trap\"", "\"allow\"", &format!("[{rules}]")));
    assert!(input.len() < MAX_JSON_BYTES);
    assert_eq!(
        compile_json(&input, TargetArch::X86_64, CompileOptions::new()).err(),
        Some(CompileError::ProgramTooLarge)
    );
}
