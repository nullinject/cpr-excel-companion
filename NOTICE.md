Protocol conversion is ported from Kaixxrua/excel-codex-bridge, commit 8a277dfcdbb647d2ef4d714e31b6a98260a63a79, under the Unlicense. The project root LICENSE preserves that license.

vendor/gateway-plugin-sdk contains the public SDK from zyycn/codex-proxy-rs v3.16.0, commit 0534dd8f2679e6f2d08a4f6df1b79abc5cbd4716, Apache-2.0. Its Cargo metadata is adjusted to build outside the upstream workspace. The upstream license is preserved in vendor/gateway-plugin-sdk/LICENSE.

The companion and adapter do not depend on modified CPR host modules. This does not by itself establish runtime compatibility; see README.md for validation status.
