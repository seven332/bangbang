//! Minimal PSCI-over-HVC responder for the single-vCPU arm64 boot path.

const PSCI_VERSION: u64 = 0x8400_0000;
const PSCI_MIGRATE_INFO_TYPE: u64 = 0x8400_0006;
const PSCI_SYSTEM_OFF: u64 = 0x8400_0008;
const PSCI_SYSTEM_RESET: u64 = 0x8400_0009;
const PSCI_FEATURES: u64 = 0x8400_000a;
const PSCI_VERSION_0_2: u64 = 0x0000_0002;
const PSCI_RET_SUCCESS: u64 = 0;
const PSCI_RET_NOT_SUPPORTED: u64 = u64::MAX;
const PSCI_MIGRATE_INFO_TYPE_TRUSTED_OS_NOT_REQUIRED: u64 = 2;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct PsciCall {
    function_id: u64,
    arg0: u64,
}

impl PsciCall {
    pub(crate) const fn new(function_id: u64, arg0: u64) -> Self {
        Self { function_id, arg0 }
    }
}

pub(crate) const fn call_uses_arg0(function_id: u64) -> bool {
    matches!(function_id, PSCI_FEATURES)
}

pub(crate) const fn not_supported_result() -> PsciCallResult {
    PsciCallResult {
        return_value: PSCI_RET_NOT_SUPPORTED,
        action: PsciCallAction::Return,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PsciCallAction {
    Return,
    SystemOff,
    SystemReset,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct PsciCallResult {
    return_value: u64,
    action: PsciCallAction,
}

impl PsciCallResult {
    pub(crate) const fn return_value(self) -> u64 {
        self.return_value
    }

    pub(crate) const fn action(self) -> PsciCallAction {
        self.action
    }
}

pub(crate) const fn handle_call(call: PsciCall) -> PsciCallResult {
    let (return_value, action) = match call.function_id {
        PSCI_VERSION => (PSCI_VERSION_0_2, PsciCallAction::Return),
        PSCI_MIGRATE_INFO_TYPE => (
            PSCI_MIGRATE_INFO_TYPE_TRUSTED_OS_NOT_REQUIRED,
            PsciCallAction::Return,
        ),
        PSCI_SYSTEM_OFF => (PSCI_RET_SUCCESS, PsciCallAction::SystemOff),
        PSCI_SYSTEM_RESET => (PSCI_RET_SUCCESS, PsciCallAction::SystemReset),
        PSCI_FEATURES => {
            if supports_function(call.arg0) {
                (PSCI_RET_SUCCESS, PsciCallAction::Return)
            } else {
                (PSCI_RET_NOT_SUPPORTED, PsciCallAction::Return)
            }
        }
        _ => (PSCI_RET_NOT_SUPPORTED, PsciCallAction::Return),
    };

    PsciCallResult {
        return_value,
        action,
    }
}

const fn supports_function(function_id: u64) -> bool {
    matches!(
        function_id,
        PSCI_VERSION | PSCI_MIGRATE_INFO_TYPE | PSCI_SYSTEM_OFF | PSCI_SYSTEM_RESET | PSCI_FEATURES
    )
}

#[cfg(test)]
mod tests {
    use super::{
        PSCI_FEATURES, PSCI_MIGRATE_INFO_TYPE, PSCI_MIGRATE_INFO_TYPE_TRUSTED_OS_NOT_REQUIRED,
        PSCI_RET_NOT_SUPPORTED, PSCI_RET_SUCCESS, PSCI_SYSTEM_OFF, PSCI_SYSTEM_RESET, PSCI_VERSION,
        PSCI_VERSION_0_2, PsciCall, PsciCallAction, call_uses_arg0, handle_call,
        not_supported_result,
    };

    #[test]
    fn returns_psci_version_0_2() {
        assert_eq!(
            handle_call(PsciCall::new(PSCI_VERSION, 0)).return_value(),
            PSCI_VERSION_0_2
        );
    }

    #[test]
    fn returns_trusted_os_migration_not_required_for_migrate_info_type() {
        assert_eq!(
            handle_call(PsciCall::new(PSCI_MIGRATE_INFO_TYPE, 0)).return_value(),
            PSCI_MIGRATE_INFO_TYPE_TRUSTED_OS_NOT_REQUIRED
        );
    }

    #[test]
    fn reports_features_for_supported_functions() {
        for function_id in [
            PSCI_VERSION,
            PSCI_MIGRATE_INFO_TYPE,
            PSCI_SYSTEM_OFF,
            PSCI_SYSTEM_RESET,
            PSCI_FEATURES,
        ] {
            assert_eq!(
                handle_call(PsciCall::new(PSCI_FEATURES, function_id)).return_value(),
                PSCI_RET_SUCCESS
            );
        }
    }

    #[test]
    fn identifies_calls_that_use_arg0() {
        assert!(call_uses_arg0(PSCI_FEATURES));
        assert!(!call_uses_arg0(PSCI_VERSION));
        assert!(!call_uses_arg0(PSCI_MIGRATE_INFO_TYPE));
        assert!(!call_uses_arg0(PSCI_SYSTEM_OFF));
        assert!(!call_uses_arg0(PSCI_SYSTEM_RESET));
        assert!(!call_uses_arg0(0x8400_0003));
    }

    #[test]
    fn builds_not_supported_result() {
        let result = not_supported_result();

        assert_eq!(result.return_value(), PSCI_RET_NOT_SUPPORTED);
        assert_eq!(result.action(), PsciCallAction::Return);
    }

    #[test]
    fn classifies_system_off_as_terminal_action() {
        let result = handle_call(PsciCall::new(PSCI_SYSTEM_OFF, 0));

        assert_eq!(result.return_value(), PSCI_RET_SUCCESS);
        assert_eq!(result.action(), PsciCallAction::SystemOff);
    }

    #[test]
    fn classifies_system_reset_as_terminal_action() {
        let result = handle_call(PsciCall::new(PSCI_SYSTEM_RESET, 0));

        assert_eq!(result.return_value(), PSCI_RET_SUCCESS);
        assert_eq!(result.action(), PsciCallAction::SystemReset);
    }

    #[test]
    fn reports_not_supported_for_unsupported_features() {
        for function_id in [0x8400_0001, 0x8400_0003, 0xc400_0003, 0xffff_ffff] {
            assert_eq!(
                handle_call(PsciCall::new(PSCI_FEATURES, function_id)).return_value(),
                PSCI_RET_NOT_SUPPORTED
            );
        }
    }

    #[test]
    fn returns_not_supported_for_unsupported_calls() {
        for function_id in [0x8400_0001, 0x8400_0002, 0x8400_0003, 0xc400_0003] {
            assert_eq!(
                handle_call(PsciCall::new(function_id, 0)).return_value(),
                PSCI_RET_NOT_SUPPORTED
            );
        }
    }
}
