#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BalloonConfigInput {
    amount_mib: u32,
    deflate_on_oom: bool,
    stats_polling_interval_s: u16,
    free_page_hinting: bool,
    free_page_reporting: bool,
}

impl BalloonConfigInput {
    pub const fn new(amount_mib: u32, deflate_on_oom: bool) -> Self {
        Self {
            amount_mib,
            deflate_on_oom,
            stats_polling_interval_s: 0,
            free_page_hinting: false,
            free_page_reporting: false,
        }
    }

    pub const fn with_stats_polling_interval_s(mut self, stats_polling_interval_s: u16) -> Self {
        self.stats_polling_interval_s = stats_polling_interval_s;
        self
    }

    pub const fn with_free_page_hinting(mut self, free_page_hinting: bool) -> Self {
        self.free_page_hinting = free_page_hinting;
        self
    }

    pub const fn with_free_page_reporting(mut self, free_page_reporting: bool) -> Self {
        self.free_page_reporting = free_page_reporting;
        self
    }

    pub const fn amount_mib(self) -> u32 {
        self.amount_mib
    }

    pub const fn deflate_on_oom(self) -> bool {
        self.deflate_on_oom
    }

    pub const fn stats_polling_interval_s(self) -> u16 {
        self.stats_polling_interval_s
    }

    pub const fn free_page_hinting(self) -> bool {
        self.free_page_hinting
    }

    pub const fn free_page_reporting(self) -> bool {
        self.free_page_reporting
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BalloonConfig {
    amount_mib: u32,
    deflate_on_oom: bool,
    stats_polling_interval_s: u16,
    free_page_hinting: bool,
    free_page_reporting: bool,
}

impl BalloonConfig {
    pub const fn amount_mib(self) -> u32 {
        self.amount_mib
    }

    pub const fn deflate_on_oom(self) -> bool {
        self.deflate_on_oom
    }

    pub const fn stats_polling_interval_s(self) -> u16 {
        self.stats_polling_interval_s
    }

    pub const fn free_page_hinting(self) -> bool {
        self.free_page_hinting
    }

    pub const fn free_page_reporting(self) -> bool {
        self.free_page_reporting
    }
}

impl From<BalloonConfigInput> for BalloonConfig {
    fn from(input: BalloonConfigInput) -> Self {
        Self {
            amount_mib: input.amount_mib,
            deflate_on_oom: input.deflate_on_oom,
            stats_polling_interval_s: input.stats_polling_interval_s,
            free_page_hinting: input.free_page_hinting,
            free_page_reporting: input.free_page_reporting,
        }
    }
}
