import XCTest

@testable import SubcFed

/// Measures whether the Swift fed-wire hello decoder tolerates a header field it
/// has never heard of.
///
/// CALLO needs to add a hub-identity field to the hello for the NATS plane's
/// roster column, and recorded the iOS decoder's tolerance as UNMEASURED —
/// correctly, since reading `parseRemoteHello` shows keyed lookups and no
/// key-set enumeration, and "I read the code" is not "I ran it".
///
/// The baseline arm is not ceremony. The first version of this file used
/// `"inc-1"` as the incarnation; the decoder requires a lowercase UUID, so ALL
/// THREE arms threw `fed_limits_unsupported` and a reader with only the positive
/// arms would have concluded the decoder REFUSES unknown fields. The control is
/// what separates "the unknown field was rejected" from "my fixture was invalid
/// for an unrelated reason".
final class FedHelloUnknownFieldTests: XCTestCase {
    private func helloFrame(extraField: (String, FedJSONValue)?) -> FedFrame {
        var header: [String: FedJSONValue] = [
            "versions": .array([.integer(1)]),
            "features": .array([]),
            "max_body_bytes": .integer(1_048_576),
            "max_in_flight": .integer(16),
            "keepalive_interval_ms": .integer(30_000),
            // Must be a lowercase UUID (FedHello.swift:93-98).
            "incarnation": .string("6f1c2a54-9b3d-4e7a-8c15-2d0f7b6e4a91"),
            "ledger_epoch": .string("epoch-1"),
            "device_name": .string("test-device"),
        ]
        if let (key, value) = extraField {
            header[key] = value
        }
        return FedFrame(type: "hello", fields: header)
    }

    /// CONTROL: the same hello WITHOUT the extra field must decode, or neither
    /// arm below says anything about the unknown field specifically.
    func testBaselineHelloDecodes() throws {
        let parsed = try FedHelloCodec.parseRemoteHello(helloFrame(extraField: nil))
        XCTAssertEqual(parsed.deviceName, "test-device")
        XCTAssertEqual(parsed.versions, [1])
    }

    /// The measurement CALLO needs: an unrecognised header field is ignored
    /// rather than refused, so the hello may gain one without a producer-last
    /// tolerance release on the Swift side first.
    func testHelloWithAnUnknownStringFieldStillDecodes() throws {
        let parsed = try FedHelloCodec.parseRemoteHello(
            helloFrame(extraField: ("nats_hub_identity", .string("nkey-ABCDEF")))
        )
        XCTAssertEqual(
            parsed.deviceName, "test-device",
            "an unknown hello field must not disturb the fields the decoder does read"
        )
        XCTAssertEqual(parsed.versions, [1])
    }

    /// A second shape, because tolerance to a string says nothing about
    /// tolerance to a nested object — the likelier shape if a hub identity ever
    /// carries more than one value.
    func testHelloWithAnUnknownObjectFieldStillDecodes() throws {
        let parsed = try FedHelloCodec.parseRemoteHello(
            helloFrame(
                extraField: (
                    "nats_hub",
                    .object([
                        "identity": .string("nkey-ABCDEF"),
                        "port": .integer(4222),
                    ])
                )
            )
        )
        XCTAssertEqual(parsed.deviceName, "test-device")
    }
}
