#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Endpoint {
    DescribeInstance,
    Version,
    VmConfig,
    Actions,
    BootSource,
    MachineConfig,
    Drive,
    NetworkInterface,
    Vsock,
}
