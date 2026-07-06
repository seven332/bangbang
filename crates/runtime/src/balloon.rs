//! Backend-neutral virtio-balloon configuration model.

use std::fmt;

pub const VIRTIO_BALLOON_DEVICE_ID: u32 = 5;
pub const VIRTIO_BALLOON_QUEUE_SIZE: u16 = 256;
pub const VIRTIO_BALLOON_MIN_QUEUE_COUNT: usize = 2;
pub const VIRTIO_BALLOON_MAX_QUEUE_COUNT: usize = 5;
pub const VIRTIO_BALLOON_INFLATE_QUEUE_INDEX: usize = 0;
pub const VIRTIO_BALLOON_DEFLATE_QUEUE_INDEX: usize = 1;
pub const VIRTIO_BALLOON_STATS_QUEUE_INDEX: usize = 2;
pub const VIRTIO_BALLOON_MIB_TO_4K_PAGES: u32 = 256;
pub const VIRTIO_BALLOON_MAX_AMOUNT_MIB: u32 = u32::MAX / VIRTIO_BALLOON_MIB_TO_4K_PAGES;
pub const VIRTIO_BALLOON_CONFIG_SPACE_SIZE: usize = 12;
pub const VIRTIO_BALLOON_FREE_PAGE_HINT_STOP: u32 = 0;
pub const VIRTIO_BALLOON_FREE_PAGE_HINT_DONE: u32 = 1;
pub const VIRTIO_BALLOON_F_STATS_VQ: u32 = 1;
pub const VIRTIO_BALLOON_F_DEFLATE_ON_OOM: u32 = 2;
pub const VIRTIO_BALLOON_F_FREE_PAGE_HINTING: u32 = 3;
pub const VIRTIO_BALLOON_F_FREE_PAGE_REPORTING: u32 = 5;
pub const VIRTIO_FEATURE_VERSION_1: u32 = 32;

const fn virtio_feature_bit(feature: u32) -> u64 {
    1_u64 << feature
}

pub fn mib_to_4k_pages(amount_mib: u32) -> Result<u32, BalloonPageCountOverflow> {
    amount_mib
        .checked_mul(VIRTIO_BALLOON_MIB_TO_4K_PAGES)
        .ok_or(BalloonPageCountOverflow { amount_mib })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BalloonPageCountOverflow {
    amount_mib: u32,
}

impl BalloonPageCountOverflow {
    pub const fn amount_mib(self) -> u32 {
        self.amount_mib
    }

    pub const fn max_amount_mib(self) -> u32 {
        VIRTIO_BALLOON_MAX_AMOUNT_MIB
    }
}

impl fmt::Display for BalloonPageCountOverflow {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "balloon amount_mib {} exceeds maximum {} MiB representable as 4 KiB pages",
            self.amount_mib,
            self.max_amount_mib()
        )
    }
}

impl std::error::Error for BalloonPageCountOverflow {}

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

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct VirtioBalloonConfigSpace {
    num_pages: u32,
    actual_pages: u32,
    free_page_hint_cmd_id: u32,
}

impl VirtioBalloonConfigSpace {
    pub const fn new(num_pages: u32, actual_pages: u32, free_page_hint_cmd_id: u32) -> Self {
        Self {
            num_pages,
            actual_pages,
            free_page_hint_cmd_id,
        }
    }

    pub fn from_config(config: BalloonConfig) -> Result<Self, BalloonPageCountOverflow> {
        Ok(Self::new(
            mib_to_4k_pages(config.amount_mib())?,
            0,
            VIRTIO_BALLOON_FREE_PAGE_HINT_STOP,
        ))
    }

    pub const fn num_pages(self) -> u32 {
        self.num_pages
    }

    pub const fn actual_pages(self) -> u32 {
        self.actual_pages
    }

    pub const fn free_page_hint_cmd_id(self) -> u32 {
        self.free_page_hint_cmd_id
    }

    pub fn to_le_bytes(self) -> [u8; VIRTIO_BALLOON_CONFIG_SPACE_SIZE] {
        let [num_pages0, num_pages1, num_pages2, num_pages3] = self.num_pages.to_le_bytes();
        let [actual_pages0, actual_pages1, actual_pages2, actual_pages3] =
            self.actual_pages.to_le_bytes();
        let [cmd0, cmd1, cmd2, cmd3] = self.free_page_hint_cmd_id.to_le_bytes();

        [
            num_pages0,
            num_pages1,
            num_pages2,
            num_pages3,
            actual_pages0,
            actual_pages1,
            actual_pages2,
            actual_pages3,
            cmd0,
            cmd1,
            cmd2,
            cmd3,
        ]
    }
}

impl TryFrom<BalloonConfig> for VirtioBalloonConfigSpace {
    type Error = BalloonPageCountOverflow;

    fn try_from(config: BalloonConfig) -> Result<Self, Self::Error> {
        Self::from_config(config)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VirtioBalloonQueueKind {
    Inflate,
    Deflate,
    Statistics,
    FreePageHinting,
    FreePageReporting,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VirtioBalloonQueueConfig {
    kind: VirtioBalloonQueueKind,
    index: usize,
    size: u16,
}

impl VirtioBalloonQueueConfig {
    pub const fn new(kind: VirtioBalloonQueueKind, index: usize, size: u16) -> Self {
        Self { kind, index, size }
    }

    pub const fn kind(self) -> VirtioBalloonQueueKind {
        self.kind
    }

    pub const fn index(self) -> usize {
        self.index
    }

    pub const fn size(self) -> u16 {
        self.size
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VirtioBalloonQueueLayout {
    inflate: VirtioBalloonQueueConfig,
    deflate: VirtioBalloonQueueConfig,
    statistics: Option<VirtioBalloonQueueConfig>,
    free_page_hinting: Option<VirtioBalloonQueueConfig>,
    free_page_reporting: Option<VirtioBalloonQueueConfig>,
}

impl VirtioBalloonQueueLayout {
    pub fn from_config(config: BalloonConfig) -> Self {
        let inflate = VirtioBalloonQueueConfig::new(
            VirtioBalloonQueueKind::Inflate,
            VIRTIO_BALLOON_INFLATE_QUEUE_INDEX,
            VIRTIO_BALLOON_QUEUE_SIZE,
        );
        let deflate = VirtioBalloonQueueConfig::new(
            VirtioBalloonQueueKind::Deflate,
            VIRTIO_BALLOON_DEFLATE_QUEUE_INDEX,
            VIRTIO_BALLOON_QUEUE_SIZE,
        );
        let mut next_queue_index = VIRTIO_BALLOON_STATS_QUEUE_INDEX;

        let statistics = if config.stats_polling_interval_s() > 0 {
            let queue = VirtioBalloonQueueConfig::new(
                VirtioBalloonQueueKind::Statistics,
                next_queue_index,
                VIRTIO_BALLOON_QUEUE_SIZE,
            );
            next_queue_index += 1;
            Some(queue)
        } else {
            None
        };

        let free_page_hinting = if config.free_page_hinting() {
            let queue = VirtioBalloonQueueConfig::new(
                VirtioBalloonQueueKind::FreePageHinting,
                next_queue_index,
                VIRTIO_BALLOON_QUEUE_SIZE,
            );
            next_queue_index += 1;
            Some(queue)
        } else {
            None
        };

        let free_page_reporting = if config.free_page_reporting() {
            Some(VirtioBalloonQueueConfig::new(
                VirtioBalloonQueueKind::FreePageReporting,
                next_queue_index,
                VIRTIO_BALLOON_QUEUE_SIZE,
            ))
        } else {
            None
        };

        Self {
            inflate,
            deflate,
            statistics,
            free_page_hinting,
            free_page_reporting,
        }
    }

    pub const fn inflate(self) -> VirtioBalloonQueueConfig {
        self.inflate
    }

    pub const fn deflate(self) -> VirtioBalloonQueueConfig {
        self.deflate
    }

    pub const fn statistics(self) -> Option<VirtioBalloonQueueConfig> {
        self.statistics
    }

    pub const fn free_page_hinting(self) -> Option<VirtioBalloonQueueConfig> {
        self.free_page_hinting
    }

    pub const fn free_page_reporting(self) -> Option<VirtioBalloonQueueConfig> {
        self.free_page_reporting
    }

    pub fn queue_count(self) -> usize {
        self.iter().count()
    }

    pub fn iter(self) -> impl Iterator<Item = VirtioBalloonQueueConfig> {
        [
            Some(self.inflate),
            Some(self.deflate),
            self.statistics,
            self.free_page_hinting,
            self.free_page_reporting,
        ]
        .into_iter()
        .flatten()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PreparedBalloonDevice {
    config_space: VirtioBalloonConfigSpace,
    available_features: u64,
    queue_layout: VirtioBalloonQueueLayout,
}

impl PreparedBalloonDevice {
    pub fn from_config(config: BalloonConfig) -> Result<Self, BalloonPageCountOverflow> {
        Ok(Self {
            config_space: VirtioBalloonConfigSpace::from_config(config)?,
            available_features: available_features(config),
            queue_layout: VirtioBalloonQueueLayout::from_config(config),
        })
    }

    pub const fn config_space(self) -> VirtioBalloonConfigSpace {
        self.config_space
    }

    pub const fn available_features(self) -> u64 {
        self.available_features
    }

    pub const fn queue_layout(self) -> VirtioBalloonQueueLayout {
        self.queue_layout
    }
}

impl TryFrom<BalloonConfig> for PreparedBalloonDevice {
    type Error = BalloonPageCountOverflow;

    fn try_from(config: BalloonConfig) -> Result<Self, Self::Error> {
        Self::from_config(config)
    }
}

pub const fn available_features(config: BalloonConfig) -> u64 {
    let mut features = virtio_feature_bit(VIRTIO_FEATURE_VERSION_1);
    if config.deflate_on_oom() {
        features |= virtio_feature_bit(VIRTIO_BALLOON_F_DEFLATE_ON_OOM);
    }
    if config.stats_polling_interval_s() > 0 {
        features |= virtio_feature_bit(VIRTIO_BALLOON_F_STATS_VQ);
    }
    if config.free_page_hinting() {
        features |= virtio_feature_bit(VIRTIO_BALLOON_F_FREE_PAGE_HINTING);
    }
    if config.free_page_reporting() {
        features |= virtio_feature_bit(VIRTIO_BALLOON_F_FREE_PAGE_REPORTING);
    }

    features
}

#[cfg(test)]
mod tests {
    use super::*;

    fn balloon_config(
        amount_mib: u32,
        deflate_on_oom: bool,
        stats_polling_interval_s: u16,
        free_page_hinting: bool,
        free_page_reporting: bool,
    ) -> BalloonConfig {
        BalloonConfigInput::new(amount_mib, deflate_on_oom)
            .with_stats_polling_interval_s(stats_polling_interval_s)
            .with_free_page_hinting(free_page_hinting)
            .with_free_page_reporting(free_page_reporting)
            .into()
    }

    fn prepared(config: BalloonConfig) -> PreparedBalloonDevice {
        PreparedBalloonDevice::from_config(config).expect("balloon config should prepare")
    }

    fn has_feature(features: u64, feature: u32) -> bool {
        features & virtio_feature_bit(feature) != 0
    }

    #[test]
    fn default_prepared_device_has_version_feature_and_base_queues() {
        let device = prepared(balloon_config(64, false, 0, false, false));

        assert_eq!(
            device.available_features(),
            virtio_feature_bit(VIRTIO_FEATURE_VERSION_1)
        );
        assert_eq!(
            device.queue_layout().queue_count(),
            VIRTIO_BALLOON_MIN_QUEUE_COUNT
        );
        let queues: Vec<_> = device.queue_layout().iter().collect();
        assert_eq!(
            queues,
            vec![
                VirtioBalloonQueueConfig::new(
                    VirtioBalloonQueueKind::Inflate,
                    VIRTIO_BALLOON_INFLATE_QUEUE_INDEX,
                    VIRTIO_BALLOON_QUEUE_SIZE,
                ),
                VirtioBalloonQueueConfig::new(
                    VirtioBalloonQueueKind::Deflate,
                    VIRTIO_BALLOON_DEFLATE_QUEUE_INDEX,
                    VIRTIO_BALLOON_QUEUE_SIZE,
                ),
            ]
        );
    }

    #[test]
    fn prepared_device_features_follow_balloon_config() {
        let device = prepared(balloon_config(64, true, 1, true, true));
        let features = device.available_features();

        assert!(has_feature(features, VIRTIO_FEATURE_VERSION_1));
        assert!(has_feature(features, VIRTIO_BALLOON_F_DEFLATE_ON_OOM));
        assert!(has_feature(features, VIRTIO_BALLOON_F_STATS_VQ));
        assert!(has_feature(features, VIRTIO_BALLOON_F_FREE_PAGE_HINTING));
        assert!(has_feature(features, VIRTIO_BALLOON_F_FREE_PAGE_REPORTING));
    }

    #[test]
    fn prepared_device_omits_disabled_optional_features() {
        let device = prepared(balloon_config(64, true, 0, false, true));
        let features = device.available_features();

        assert!(has_feature(features, VIRTIO_FEATURE_VERSION_1));
        assert!(has_feature(features, VIRTIO_BALLOON_F_DEFLATE_ON_OOM));
        assert!(!has_feature(features, VIRTIO_BALLOON_F_STATS_VQ));
        assert!(!has_feature(features, VIRTIO_BALLOON_F_FREE_PAGE_HINTING));
        assert!(has_feature(features, VIRTIO_BALLOON_F_FREE_PAGE_REPORTING));
    }

    #[test]
    fn prepared_device_converts_target_mib_to_config_pages() {
        let device = prepared(balloon_config(64, false, 0, false, false));
        let config_space = device.config_space();

        assert_eq!(
            config_space.num_pages(),
            64 * VIRTIO_BALLOON_MIB_TO_4K_PAGES
        );
        assert_eq!(config_space.actual_pages(), 0);
        assert_eq!(
            config_space.free_page_hint_cmd_id(),
            VIRTIO_BALLOON_FREE_PAGE_HINT_STOP
        );
    }

    #[test]
    fn page_conversion_rejects_overflow() {
        let amount_mib = VIRTIO_BALLOON_MAX_AMOUNT_MIB + 1;
        let err = mib_to_4k_pages(amount_mib).expect_err("page conversion should overflow");

        assert_eq!(err.amount_mib(), amount_mib);
        assert_eq!(err.max_amount_mib(), VIRTIO_BALLOON_MAX_AMOUNT_MIB);
        assert_eq!(
            err.to_string(),
            "balloon amount_mib 16777216 exceeds maximum 16777215 MiB representable as 4 KiB pages"
        );
        assert_eq!(
            PreparedBalloonDevice::from_config(balloon_config(amount_mib, false, 0, false, false))
                .expect_err("prepared device should reject page overflow"),
            err
        );
    }

    #[test]
    fn page_conversion_accepts_maximum_amount() {
        let pages = mib_to_4k_pages(VIRTIO_BALLOON_MAX_AMOUNT_MIB)
            .expect("maximum balloon amount should convert");
        let device = prepared(balloon_config(
            VIRTIO_BALLOON_MAX_AMOUNT_MIB,
            false,
            0,
            false,
            false,
        ));

        assert_eq!(pages, u32::MAX - 255);
        assert_eq!(device.config_space().num_pages(), pages);
    }

    #[test]
    fn config_space_uses_firecracker_little_endian_layout() {
        let config_space = VirtioBalloonConfigSpace::new(0x0102_0304, 0x0506_0708, 0x090a_0b0c);

        assert_eq!(
            config_space.to_le_bytes(),
            [
                0x04, 0x03, 0x02, 0x01, 0x08, 0x07, 0x06, 0x05, 0x0c, 0x0b, 0x0a, 0x09,
            ]
        );
        assert_eq!(
            config_space.to_le_bytes().len(),
            VIRTIO_BALLOON_CONFIG_SPACE_SIZE
        );
    }

    #[test]
    fn optional_queue_metadata_is_deterministic_and_bounded() {
        let device = prepared(balloon_config(64, false, 1, true, true));
        let queues: Vec<_> = device.queue_layout().iter().collect();

        assert_eq!(queues.len(), VIRTIO_BALLOON_MAX_QUEUE_COUNT);
        assert_eq!(
            queues,
            vec![
                VirtioBalloonQueueConfig::new(
                    VirtioBalloonQueueKind::Inflate,
                    VIRTIO_BALLOON_INFLATE_QUEUE_INDEX,
                    VIRTIO_BALLOON_QUEUE_SIZE,
                ),
                VirtioBalloonQueueConfig::new(
                    VirtioBalloonQueueKind::Deflate,
                    VIRTIO_BALLOON_DEFLATE_QUEUE_INDEX,
                    VIRTIO_BALLOON_QUEUE_SIZE,
                ),
                VirtioBalloonQueueConfig::new(
                    VirtioBalloonQueueKind::Statistics,
                    VIRTIO_BALLOON_STATS_QUEUE_INDEX,
                    VIRTIO_BALLOON_QUEUE_SIZE,
                ),
                VirtioBalloonQueueConfig::new(
                    VirtioBalloonQueueKind::FreePageHinting,
                    VIRTIO_BALLOON_STATS_QUEUE_INDEX + 1,
                    VIRTIO_BALLOON_QUEUE_SIZE,
                ),
                VirtioBalloonQueueConfig::new(
                    VirtioBalloonQueueKind::FreePageReporting,
                    VIRTIO_BALLOON_STATS_QUEUE_INDEX + 2,
                    VIRTIO_BALLOON_QUEUE_SIZE,
                ),
            ]
        );
    }

    #[test]
    fn optional_queues_compact_when_statistics_queue_is_disabled() {
        let device = prepared(balloon_config(64, false, 0, true, true));
        let queues: Vec<_> = device.queue_layout().iter().collect();

        assert_eq!(queues.len(), VIRTIO_BALLOON_MIN_QUEUE_COUNT + 2);
        assert_eq!(queues[2].kind(), VirtioBalloonQueueKind::FreePageHinting);
        assert_eq!(queues[2].index(), VIRTIO_BALLOON_STATS_QUEUE_INDEX);
        assert_eq!(queues[3].kind(), VirtioBalloonQueueKind::FreePageReporting);
        assert_eq!(queues[3].index(), VIRTIO_BALLOON_STATS_QUEUE_INDEX + 1);
    }
}
