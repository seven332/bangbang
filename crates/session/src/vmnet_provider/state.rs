mod control;
mod data;

use crate::SessionId;

pub use control::{
    ControlBrokerEvent, ControlBrokerState, ControlClientEvent, ControlClientState,
    VmnetControlBroker, VmnetControlClient,
};
pub use data::{
    DataClientEvent, DataClientState, DataOwnerEvent, DataOwnerState, VmnetDataClient,
    VmnetDataOwner,
};

use super::{VmnetProviderError, VmnetSequence};

fn require_session(session: SessionId) -> Result<(), VmnetProviderError> {
    if session.is_pre_session() {
        Err(VmnetProviderError::InvalidConfiguration)
    } else {
        Ok(())
    }
}

fn first_sequence() -> VmnetSequence {
    VmnetSequence::MIN
}

#[cfg(test)]
mod tests {
    use std::io::Read;
    use std::os::unix::net::UnixStream;

    use super::*;
    use crate::vmnet_provider::{
        ControlMessage, DataMessage, ProviderCancelReason, ProviderCleanup, ProviderEnvelope,
        ProviderFrame, RealizedVmnetParameters, RequestedVmnetParameters, VmnetGeneration,
        VmnetInterfaceId, VmnetPacketBatch, VmnetPolicySlot, VmnetReadinessEpoch, VmnetSequence,
    };

    fn session(value: u8) -> SessionId {
        SessionId::from_bytes([value; 32])
    }

    fn interface(value: u32) -> VmnetInterfaceId {
        VmnetInterfaceId::new(value).expect("interface should be nonzero")
    }

    fn generation(value: u64) -> VmnetGeneration {
        VmnetGeneration::new(value).expect("generation should be nonzero")
    }

    fn sequence(value: u64) -> VmnetSequence {
        VmnetSequence::new(value).expect("sequence should be nonzero")
    }

    fn realized() -> RealizedVmnetParameters {
        RealizedVmnetParameters::new([2, 0, 0, 0, 0, 1], 1500, 2048)
            .expect("base parameters should validate")
            .with_backend_interface_id(Some([7; 16]))
            .expect("identity should validate")
            .with_direct_virtio_header(true, true)
            .expect("direct header should validate")
            .with_batch_limits(Some(8), Some(8))
            .expect("parameters should be valid")
    }

    fn control_pair() -> (VmnetControlClient, VmnetControlBroker) {
        let mut client = VmnetControlClient::new(session(1)).expect("client should construct");
        let mut broker = VmnetControlBroker::new(session(1)).expect("broker should construct");
        assert!(matches!(
            broker.receive(client.hello().expect("hello should emit")),
            Ok(ControlBrokerEvent::Hello)
        ));
        assert!(matches!(
            client.receive(broker.hello_ack().expect("ack should emit")),
            Ok(ControlClientEvent::Ready)
        ));
        (client, broker)
    }

    fn data_pair() -> (VmnetDataClient, VmnetDataOwner) {
        let mut client = VmnetDataClient::new(session(2), interface(1), generation(1), realized())
            .expect("client should construct");
        let mut owner = VmnetDataOwner::new(session(2), interface(1), generation(1), realized())
            .expect("owner should construct");
        assert_eq!(
            owner.receive(client.hello().expect("hello should emit")),
            Ok(DataOwnerEvent::Hello)
        );
        assert_eq!(
            client.receive(owner.hello_ack().expect("ack should emit")),
            Ok(DataClientEvent::Ready)
        );
        (client, owner)
    }

    #[test]
    fn control_supports_four_interfaces_reuse_and_orderly_shutdown() {
        let (mut client, mut broker) = control_pair();
        let requested =
            RequestedVmnetParameters::new(None, Some(1500)).expect("request should be valid");
        let mut peers = Vec::new();
        for value in 1_u32..=4 {
            let id = interface(value);
            let request = client
                .start(id, VmnetPolicySlot::Shared, requested)
                .expect("start should emit");
            assert!(matches!(
                broker.receive(request),
                Ok(ControlBrokerEvent::Start { interface, .. }) if interface == id
            ));
            let (data, peer) = UnixStream::pair().expect("pair should construct");
            peers.push(peer);
            let response = broker
                .started(generation(u64::from(value)), realized(), data)
                .expect("started should emit");
            assert!(matches!(
                client.receive(response),
                Ok(ControlClientEvent::Started {
                    interface,
                    generation: active_generation,
                    ..
                }) if interface == id && active_generation == generation(u64::from(value))
            ));
        }
        assert_eq!(client.active_count(), 4);
        assert_eq!(broker.active_count(), 4);
        assert!(matches!(
            client.start(interface(5), VmnetPolicySlot::Host, requested),
            Err(VmnetProviderError::LimitExceeded)
        ));

        let first = interface(1);
        assert!(matches!(
            broker.receive(client.stop(first).expect("stop should emit")),
            Ok(ControlBrokerEvent::Stop { interface, .. }) if interface == first
        ));
        assert!(matches!(
            client.receive(
                broker
                    .stopped(ProviderCleanup::Complete)
                    .expect("stopped should emit")
            ),
            Ok(ControlClientEvent::Stopped { interface, .. }) if interface == first
        ));

        let (data, peer) = UnixStream::pair().expect("pair should construct");
        peers.push(peer);
        broker
            .receive(
                client
                    .start(first, VmnetPolicySlot::Host, requested)
                    .expect("reused start should emit"),
            )
            .expect("reused start should validate");
        assert!(matches!(
            client.receive(
                broker
                    .started(generation(5), realized(), data)
                    .expect("fresh generation should emit")
            ),
            Ok(ControlClientEvent::Started { generation: active, .. }) if active == generation(5)
        ));

        for value in [1_u32, 2, 3, 4] {
            let id = interface(value);
            broker
                .receive(client.stop(id).expect("stop should emit"))
                .expect("stop should validate");
            client
                .receive(
                    broker
                        .stopped(ProviderCleanup::Complete)
                        .expect("stopped should emit"),
                )
                .expect("stopped should validate");
        }
        assert_eq!(client.active_count(), 0);
        assert_eq!(broker.active_count(), 0);
        assert_eq!(
            broker.receive(client.shutdown().expect("shutdown should emit")),
            Ok(ControlBrokerEvent::Shutdown)
        );
        assert!(matches!(
            client.receive(broker.shutdown_ack().expect("ack should emit")),
            Ok(ControlClientEvent::Shutdown)
        ));
        assert_eq!(client.state(), ControlClientState::Closed);
        assert_eq!(broker.state(), ControlBrokerState::Closed);
        drop(peers);
    }

    #[test]
    fn cancellation_accepts_one_raced_start_but_never_exposes_its_stream() {
        let (mut client, mut broker) = control_pair();
        let id = interface(1);
        let requested = RequestedVmnetParameters::new(None, None).expect("request should validate");
        broker
            .receive(
                client
                    .start(id, VmnetPolicySlot::Host, requested)
                    .expect("start should emit"),
            )
            .expect("start should validate");
        assert!(matches!(
            broker.receive(
                client
                    .cancel(ProviderCancelReason::Worker)
                    .expect("cancel should emit")
            ),
            Ok(ControlBrokerEvent::Cancel { .. })
        ));
        let (data, mut peer) = UnixStream::pair().expect("pair should construct");
        assert!(matches!(
            client.receive(
                broker
                    .started(generation(1), realized(), data)
                    .expect("raced result should emit")
            ),
            Ok(ControlClientEvent::StartRetiredDuringCancellation { .. })
        ));
        let mut byte = [0_u8; 1];
        assert_eq!(
            peer.read(&mut byte).expect("retired stream should close"),
            0
        );
        assert!(matches!(
            client.receive(
                broker
                    .cancelled(ProviderCleanup::Complete)
                    .expect("cancelled should emit")
            ),
            Ok(ControlClientEvent::Cancelled)
        ));
        assert_eq!(client.active_count(), 0);
        assert_eq!(client.state(), ControlClientState::Closed);
    }

    #[test]
    fn cross_session_control_frame_poison_clears_ownership() {
        let mut client = VmnetControlClient::new(session(1)).expect("client should construct");
        client.hello().expect("hello should emit");
        let wrong = ProviderFrame::control(
            session(9),
            None,
            None,
            sequence(1),
            ControlMessage::HelloAck,
        )
        .expect("frame should construct");
        assert!(matches!(
            client.receive(ProviderEnvelope::frame_only(wrong)),
            Err(VmnetProviderError::InvalidPeerState)
        ));
        assert_eq!(client.state(), ControlClientState::Terminal);
        assert_eq!(client.active_count(), 0);
    }

    #[test]
    fn data_validates_readiness_read_write_prefix_stop_and_shutdown() {
        let (mut client, mut owner) = data_pair();
        assert_eq!(
            client.receive(owner.readiness(2).expect("readiness should emit")),
            Ok(DataClientEvent::Readiness {
                epoch: VmnetReadinessEpoch::new(1).expect("epoch should validate"),
                estimated_packets: 2,
            })
        );
        assert_eq!(
            owner.receive(client.read(2).expect("read should emit")),
            Ok(DataOwnerEvent::Read {
                request: sequence(2),
                max_packets: 2,
            })
        );
        assert_eq!(
            client.receive(
                owner
                    .readiness(1)
                    .expect("interleaved readiness should emit")
            ),
            Ok(DataClientEvent::Readiness {
                epoch: VmnetReadinessEpoch::new(2).expect("epoch should validate"),
                estimated_packets: 1,
            })
        );
        let first = [1_u8, 2, 3];
        let second = [4_u8, 5];
        let read_batch = VmnetPacketBatch::read(&[&first, &second]).expect("batch should validate");
        assert_eq!(
            client.receive(
                owner
                    .read_result(read_batch.clone())
                    .expect("read result should emit")
            ),
            Ok(DataClientEvent::ReadComplete {
                packets: read_batch,
            })
        );

        let write_batch =
            VmnetPacketBatch::write(&[&first, &second]).expect("write should validate");
        assert!(matches!(
            owner.receive(client.write(write_batch).expect("write should emit")),
            Ok(DataOwnerEvent::Write { request, packets })
                if request == sequence(3) && packets.packet_count() == 2
        ));
        assert!(matches!(
            owner.write_result(3),
            Err(VmnetProviderError::LimitExceeded)
        ));
        assert_eq!(
            client.receive(owner.write_result(1).expect("prefix should emit")),
            Ok(DataClientEvent::WriteComplete {
                completed_packets: 1,
            })
        );

        assert_eq!(
            owner.receive(client.stop().expect("stop should emit")),
            Ok(DataOwnerEvent::Stop)
        );
        assert_eq!(
            client.receive(
                owner
                    .stopped(ProviderCleanup::Complete)
                    .expect("stopped should emit")
            ),
            Ok(DataClientEvent::Stopped)
        );
        assert_eq!(
            owner.receive(client.shutdown().expect("shutdown should emit")),
            Ok(DataOwnerEvent::Shutdown)
        );
        assert_eq!(
            client.receive(owner.shutdown_ack().expect("ack should emit")),
            Ok(DataClientEvent::Shutdown)
        );
        assert_eq!(client.state(), DataClientState::Closed);
        assert_eq!(owner.state(), DataOwnerState::Closed);
    }

    #[test]
    fn stale_readiness_and_wrong_scope_poison_data_state() {
        let (mut client, mut owner) = data_pair();
        client
            .receive(owner.readiness(1).expect("readiness should emit"))
            .expect("readiness should validate");
        let stale = ProviderFrame::data(
            session(2),
            interface(1),
            generation(1),
            sequence(3),
            DataMessage::Readiness {
                epoch: VmnetReadinessEpoch::new(1).expect("epoch should validate"),
                estimated_packets: 1,
            },
        )
        .expect("frame should construct");
        assert_eq!(
            client.receive(ProviderEnvelope::frame_only(stale)),
            Err(VmnetProviderError::InvalidPeerState)
        );
        assert_eq!(client.state(), DataClientState::Terminal);

        let (mut client, _owner) = data_pair();
        let wrong = ProviderFrame::data(
            session(2),
            interface(2),
            generation(1),
            sequence(2),
            DataMessage::Readiness {
                epoch: VmnetReadinessEpoch::new(1).expect("epoch should validate"),
                estimated_packets: 1,
            },
        )
        .expect("frame should construct");
        assert_eq!(
            client.receive(ProviderEnvelope::frame_only(wrong)),
            Err(VmnetProviderError::InvalidPeerState)
        );
        assert_eq!(client.state(), DataClientState::Terminal);
    }

    #[test]
    fn state_debug_surfaces_are_redacted() {
        let (client, broker) = control_pair();
        let (data_client, data_owner) = data_pair();
        for debug in [
            format!("{client:?}"),
            format!("{broker:?}"),
            format!("{data_client:?}"),
            format!("{data_owner:?}"),
        ] {
            assert!(debug.contains("<redacted>"));
            assert!(!debug.contains(&session(1).private_hex()));
            assert!(!debug.contains(&session(2).private_hex()));
        }
    }
}
