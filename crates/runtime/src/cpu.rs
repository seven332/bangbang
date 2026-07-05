//! Backend-neutral CPU configuration model.

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CpuConfigInput {
    custom_template_configured: bool,
}

impl CpuConfigInput {
    pub const fn new(custom_template_configured: bool) -> Self {
        Self {
            custom_template_configured,
        }
    }

    pub const fn noop() -> Self {
        Self::new(false)
    }

    pub const fn with_custom_template() -> Self {
        Self::new(true)
    }

    pub const fn custom_template_configured(self) -> bool {
        self.custom_template_configured
    }
}
