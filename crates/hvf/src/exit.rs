//! Typed snapshots of Hypervisor.framework vCPU exits.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HvfExceptionExit {
    pub syndrome: u64,
    pub virtual_address: u64,
    pub physical_address: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HvfVcpuExit {
    Canceled,
    Exception(HvfExceptionExit),
    VtimerActivated,
    Unknown { reason: u32 },
}

impl HvfVcpuExit {
    pub(crate) fn from_raw(exit: crate::ffi::HvVcpuExit) -> Self {
        match exit.reason {
            crate::ffi::HV_EXIT_REASON_CANCELED => Self::Canceled,
            crate::ffi::HV_EXIT_REASON_EXCEPTION => Self::Exception(HvfExceptionExit {
                syndrome: exit.exception.syndrome,
                virtual_address: exit.exception.virtual_address,
                physical_address: exit.exception.physical_address,
            }),
            crate::ffi::HV_EXIT_REASON_VTIMER_ACTIVATED => Self::VtimerActivated,
            crate::ffi::HV_EXIT_REASON_UNKNOWN => Self::Unknown {
                reason: crate::ffi::HV_EXIT_REASON_UNKNOWN,
            },
            reason => Self::Unknown { reason },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{HvfExceptionExit, HvfVcpuExit};

    fn raw_exit(reason: u32) -> crate::ffi::HvVcpuExit {
        crate::ffi::HvVcpuExit {
            reason,
            exception: crate::ffi::HvVcpuExitException {
                syndrome: 0x11,
                virtual_address: 0x22,
                physical_address: 0x33,
            },
        }
    }

    #[test]
    fn converts_canceled_exit() {
        assert_eq!(
            HvfVcpuExit::from_raw(raw_exit(crate::ffi::HV_EXIT_REASON_CANCELED)),
            HvfVcpuExit::Canceled
        );
    }

    #[test]
    fn converts_exception_exit() {
        assert_eq!(
            HvfVcpuExit::from_raw(raw_exit(crate::ffi::HV_EXIT_REASON_EXCEPTION)),
            HvfVcpuExit::Exception(HvfExceptionExit {
                syndrome: 0x11,
                virtual_address: 0x22,
                physical_address: 0x33,
            })
        );
    }

    #[test]
    fn converts_vtimer_activated_exit() {
        assert_eq!(
            HvfVcpuExit::from_raw(raw_exit(crate::ffi::HV_EXIT_REASON_VTIMER_ACTIVATED)),
            HvfVcpuExit::VtimerActivated
        );
    }

    #[test]
    fn preserves_sdk_unknown_exit_reason() {
        assert_eq!(
            HvfVcpuExit::from_raw(raw_exit(crate::ffi::HV_EXIT_REASON_UNKNOWN)),
            HvfVcpuExit::Unknown {
                reason: crate::ffi::HV_EXIT_REASON_UNKNOWN
            }
        );
    }

    #[test]
    fn preserves_future_unknown_exit_reason() {
        assert_eq!(
            HvfVcpuExit::from_raw(raw_exit(99)),
            HvfVcpuExit::Unknown { reason: 99 }
        );
    }
}
