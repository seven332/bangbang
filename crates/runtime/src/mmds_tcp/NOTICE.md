# Firecracker-derived MMDS TCP core

The Rust sources in this directory are a focused semantic adaptation of the
Firecracker v1.16.0 MMDS/Dumbo TCP implementation at commit
`d83d72b710361a10294480131377b1b00b163af8`, specifically:

- `src/vmm/src/dumbo/pdu/tcp.rs`
- `src/vmm/src/dumbo/tcp/mod.rs`
- `src/vmm/src/dumbo/tcp/connection.rs`
- `src/vmm/src/dumbo/tcp/endpoint.rs`
- `src/vmm/src/dumbo/tcp/handler.rs`
- the 30-connection and 100-reset defaults in `src/vmm/src/mmds/ns.rs`

Firecracker
Copyright 2017-2020 Amazon.com, Inc. or its affiliates. All Rights Reserved.
SPDX-License-Identifier: Apache-2.0

The adaptation is distributed under the Apache License, Version 2.0, included
as `LICENSE-APACHE-2.0` in this directory. It deliberately excludes
Firecracker's Linux event loop, TAP/device model, guest-memory types,
`micro_http`, global metrics, and platform time/randomness.
