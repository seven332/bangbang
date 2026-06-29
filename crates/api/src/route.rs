#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Endpoint {
    DescribeInstance,
    Version,
    VmConfig,
    Actions,
    BootSource,
    Logger,
    MachineConfig,
    Metrics,
    Drive,
    NetworkInterface,
    Vsock,
}
