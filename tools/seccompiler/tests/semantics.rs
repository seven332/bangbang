#![allow(clippy::expect_used)]

mod support;

use bangbang_seccompiler::{CompileOptions, MAX_BPF_INSTRUCTIONS, TargetArch, compile_json};
use support::{
    ACTION_ALLOW, ACTION_KILL_PROCESS, ACTION_KILL_THREAD, ACTION_TRAP, AUDIT_ARCH_AARCH64,
    AUDIT_ARCH_X86_64, SeccompData, execute, filter, policy, program,
};

fn compile_condition(operator: &str, value_type: &str, value: u64) -> Vec<u64> {
    let rules = format!(
        r#"[{{"syscall":"read","args":[{{"index":0,"type":"{value_type}","op":{operator},"val":{value}}}]}}]"#
    );
    let input = policy(&filter("\"trap\"", "\"allow\"", &rules));
    let compiled = compile_json(&input, TargetArch::X86_64, CompileOptions::new())
        .expect("condition should compile");
    program(&compiled, "vmm").to_vec()
}

fn run_argument(program: &[u64], value: u64) -> u32 {
    execute(
        program,
        SeccompData::new(0, AUDIT_ARCH_X86_64).with_arg(0, value),
    )
}

#[test]
fn encodes_every_v116_action() {
    let cases = [
        ("\"allow\"", ACTION_ALLOW),
        (r#"{"errno":42}"#, 0x0005_002a),
        ("\"kill_thread\"", 0x0000_0000),
        ("\"kill_process\"", ACTION_KILL_PROCESS),
        ("\"log\"", 0x7ffc_0000),
        (r#"{"trace":42}"#, 0x7ff0_002a),
        ("\"trap\"", ACTION_TRAP),
    ];

    for (action, expected) in cases {
        let default = if expected == ACTION_TRAP {
            "\"allow\""
        } else {
            "\"trap\""
        };
        let input = policy(&filter(default, action, r#"[{"syscall":"read"}]"#));
        let compiled = compile_json(&input, TargetArch::X86_64, CompileOptions::new())
            .expect("action should compile");
        let bpf = program(&compiled, "vmm");
        assert_eq!(
            execute(bpf, SeccompData::new(0, AUDIT_ARCH_X86_64)),
            expected
        );
    }
}

#[test]
fn evaluates_equality_and_not_equal_across_both_words() {
    let value = 0x0000_0001_ffff_ffff;
    let equal = compile_condition("\"eq\"", "qword", value);
    assert_eq!(run_argument(&equal, value), ACTION_ALLOW);
    assert_eq!(run_argument(&equal, value - 1), ACTION_TRAP);
    assert_eq!(run_argument(&equal, value + (1_u64 << 32)), ACTION_TRAP);

    let not_equal = compile_condition("\"ne\"", "qword", value);
    assert_eq!(run_argument(&not_equal, value), ACTION_TRAP);
    assert_eq!(run_argument(&not_equal, value - 1), ACTION_ALLOW);
    assert_eq!(
        run_argument(&not_equal, value + (1_u64 << 32)),
        ACTION_ALLOW
    );
}

#[test]
fn evaluates_every_unsigned_ordering_edge() {
    let value = 0x0000_0001_ffff_ffff;
    let lower_low = value - 1;
    let greater_high = value + (1_u64 << 32);
    let lower_high = value - (1_u64 << 32);

    let ge = compile_condition("\"ge\"", "qword", value);
    assert_eq!(run_argument(&ge, value), ACTION_ALLOW);
    assert_eq!(run_argument(&ge, lower_low), ACTION_TRAP);
    assert_eq!(run_argument(&ge, greater_high), ACTION_ALLOW);
    assert_eq!(run_argument(&ge, lower_high), ACTION_TRAP);

    let gt = compile_condition("\"gt\"", "qword", value);
    assert_eq!(run_argument(&gt, value), ACTION_TRAP);
    assert_eq!(run_argument(&gt, greater_high), ACTION_ALLOW);
    assert_eq!(run_argument(&gt, lower_low), ACTION_TRAP);

    let le = compile_condition("\"le\"", "qword", value);
    assert_eq!(run_argument(&le, value), ACTION_ALLOW);
    assert_eq!(run_argument(&le, lower_low), ACTION_ALLOW);
    assert_eq!(run_argument(&le, greater_high), ACTION_TRAP);

    let lt = compile_condition("\"lt\"", "qword", value);
    assert_eq!(run_argument(&lt, value), ACTION_TRAP);
    assert_eq!(run_argument(&lt, lower_high), ACTION_ALLOW);
    assert_eq!(run_argument(&lt, greater_high), ACTION_TRAP);
}

#[test]
fn evaluates_masked_equality_and_zero_mask() {
    let mask = 0xff00_ff00_00ff_00ff_u64;
    let value = 0x1200_3400_0056_0078_u64;
    let operator = format!(r#"{{"masked_eq":{mask}}}"#);
    let masked = compile_condition(&operator, "qword", value);
    assert_eq!(
        run_argument(&masked, value ^ 0x00ff_00ff_ff00_ff00),
        ACTION_ALLOW
    );
    assert_eq!(run_argument(&masked, value ^ 1), ACTION_TRAP);

    let always = compile_condition(r#"{"masked_eq":0}"#, "qword", u64::MAX);
    assert_eq!(run_argument(&always, 0), ACTION_ALLOW);
    assert_eq!(run_argument(&always, u64::MAX), ACTION_ALLOW);
}

#[test]
fn matches_current_dword_normalization_only_for_equality() {
    let dword_eq = compile_condition("\"eq\"", "dword", 7);
    assert_eq!(run_argument(&dword_eq, 0xdead_beef_0000_0007), ACTION_ALLOW);

    let qword_eq = compile_condition("\"eq\"", "qword", 7);
    assert_eq!(run_argument(&qword_eq, 0xdead_beef_0000_0007), ACTION_TRAP);

    let dword_gt = compile_condition("\"gt\"", "dword", 7);
    assert_eq!(
        run_argument(&dword_gt, 0x0000_0001_0000_0000),
        ACTION_ALLOW,
        "v1.16 passes non-equality dword comparisons to libseccomp as qwords"
    );

    let mask = 0xffff_0000_0000_0000_u64;
    let masked = compile_condition(&format!(r#"{{"masked_eq":{mask}}}"#), "dword", mask);
    assert_eq!(run_argument(&masked, mask | 7), ACTION_ALLOW);
    assert_eq!(run_argument(&masked, 7), ACTION_TRAP);
}

#[test]
fn evaluates_the_sixth_linux_syscall_argument() {
    let rules = r#"[{"syscall":"read","args":[{"index":5,"type":"qword","op":"eq","val":7}]}]"#;
    let input = policy(&filter("\"trap\"", "\"allow\"", rules));
    let compiled = compile_json(&input, TargetArch::X86_64, CompileOptions::new())
        .expect("sixth argument should compile");
    let bpf = program(&compiled, "vmm");
    assert_eq!(
        execute(bpf, SeccompData::new(0, AUDIT_ARCH_X86_64).with_arg(5, 7),),
        ACTION_ALLOW
    );
    assert_eq!(
        execute(bpf, SeccompData::new(0, AUDIT_ARCH_X86_64)),
        ACTION_TRAP
    );
}

#[test]
fn combines_conditions_with_and_and_rules_with_or() {
    let rules = r#"[
        {"syscall":"read","args":[
            {"index":0,"type":"qword","op":"eq","val":1},
            {"index":1,"type":"qword","op":"eq","val":2}
        ]},
        {"syscall":"read","args":[
            {"index":0,"type":"qword","op":"eq","val":3}
        ]}
    ]"#;
    let input = policy(&filter("\"trap\"", "\"allow\"", rules));
    let compiled = compile_json(&input, TargetArch::X86_64, CompileOptions::new())
        .expect("rules should compile");
    let bpf = program(&compiled, "vmm");

    assert_eq!(
        execute(
            bpf,
            SeccompData::new(0, AUDIT_ARCH_X86_64)
                .with_arg(0, 1)
                .with_arg(1, 2),
        ),
        ACTION_ALLOW
    );
    assert_eq!(
        execute(
            bpf,
            SeccompData::new(0, AUDIT_ARCH_X86_64)
                .with_arg(0, 1)
                .with_arg(1, 9),
        ),
        ACTION_TRAP
    );
    assert_eq!(
        execute(bpf, SeccompData::new(0, AUDIT_ARCH_X86_64).with_arg(0, 3),),
        ACTION_ALLOW
    );
}

#[test]
fn basic_mode_drops_conditions_and_unconditional_rules_subsume() {
    let rules = r#"[
        {"syscall":"read","args":[{"index":0,"type":"qword","op":"eq","val":7}]},
        {"syscall":"read"}
    ]"#;
    let input = policy(&filter("\"trap\"", "\"allow\"", rules));
    let advanced = compile_json(&input, TargetArch::X86_64, CompileOptions::new())
        .expect("advanced policy should compile");
    assert_eq!(
        execute(
            program(&advanced, "vmm"),
            SeccompData::new(0, AUDIT_ARCH_X86_64),
        ),
        ACTION_ALLOW
    );

    let only_conditional = policy(&filter(
        "\"trap\"",
        "\"allow\"",
        r#"[{"syscall":"read","args":[{"index":0,"type":"qword","op":"eq","val":7}]}]"#,
    ));
    let basic = compile_json(
        &only_conditional,
        TargetArch::X86_64,
        CompileOptions::new().with_basic(true),
    )
    .expect("basic policy should compile");
    assert_eq!(
        execute(
            program(&basic, "vmm"),
            SeccompData::new(0, AUDIT_ARCH_X86_64),
        ),
        ACTION_ALLOW
    );
}

#[test]
fn rejects_wrong_architecture_and_x32_but_allows_tracing_sentinel() {
    let input = policy(&filter("\"trap\"", "\"allow\"", r#"[{"syscall":"read"}]"#));
    let x86 = compile_json(&input, TargetArch::X86_64, CompileOptions::new())
        .expect("x86 policy should compile");
    let bpf = program(&x86, "vmm");
    assert_eq!(
        execute(bpf, SeccompData::new(0, AUDIT_ARCH_AARCH64)),
        ACTION_KILL_THREAD
    );
    assert_eq!(
        execute(bpf, SeccompData::new(0x4000_0000, AUDIT_ARCH_X86_64)),
        ACTION_KILL_THREAD
    );
    assert_eq!(
        execute(bpf, SeccompData::new(u32::MAX, AUDIT_ARCH_X86_64)),
        ACTION_TRAP
    );
}

#[test]
fn preserves_known_pnr_as_noop_and_rejects_no_valid_dispatch_match() {
    let input = policy(&filter(
        "\"trap\"",
        "\"allow\"",
        r#"[{"syscall":"access"}]"#,
    ));
    let x86 = compile_json(&input, TargetArch::X86_64, CompileOptions::new())
        .expect("x86 access should compile");
    assert_eq!(
        execute(
            program(&x86, "vmm"),
            SeccompData::new(21, AUDIT_ARCH_X86_64),
        ),
        ACTION_ALLOW
    );

    let aarch64 = compile_json(&input, TargetArch::Aarch64, CompileOptions::new())
        .expect("aarch64 PNR should compile as a no-op");
    assert_eq!(
        execute(
            program(&aarch64, "vmm"),
            SeccompData::new(21, AUDIT_ARCH_AARCH64),
        ),
        ACTION_TRAP
    );
}

#[test]
fn compiles_representative_pinned_policy_for_both_targets() {
    let rules = r#"[
        {"syscall":"read"},
        {"syscall":"write"},
        {"syscall":"ioctl","args":[{"index":1,"type":"dword","op":"eq","val":21505}]},
        {"syscall":"futex","args":[
            {"index":1,"type":"dword","op":"eq","val":0},
            {"index":3,"type":"dword","op":"eq","val":0}
        ]}
    ]"#;
    let input = policy(&filter("\"trap\"", "\"allow\"", rules));

    for (arch, audit, read_number) in [
        (TargetArch::X86_64, AUDIT_ARCH_X86_64, 0),
        (TargetArch::Aarch64, AUDIT_ARCH_AARCH64, 63),
    ] {
        let compiled = compile_json(&input, arch, CompileOptions::new())
            .expect("representative policy should compile");
        assert_eq!(compiled.len(), 3);
        for bpf in compiled.values() {
            assert!(!bpf.is_empty());
            assert!(bpf.len() <= MAX_BPF_INSTRUCTIONS);
            assert_eq!(
                execute(bpf, SeccompData::new(read_number, audit)),
                ACTION_ALLOW
            );
        }
    }
}

#[test]
fn output_is_ordered_deterministic_and_has_explicit_u64_layout() {
    let input = policy(&filter("\"trap\"", "\"allow\"", r#"[{"syscall":"read"}]"#));
    let first = compile_json(&input, TargetArch::X86_64, CompileOptions::new())
        .expect("first compile should work");
    let second = compile_json(&input, TargetArch::X86_64, CompileOptions::new())
        .expect("second compile should work");
    assert_eq!(first, second);
    assert_eq!(
        first.keys().map(String::as_str).collect::<Vec<_>>(),
        ["api", "vcpu", "vmm"]
    );
    assert_eq!(
        program(&first, "vmm").first().copied(),
        Some(0x0000_0004_0000_0020)
    );
}
