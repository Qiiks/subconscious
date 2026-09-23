import Foundation

/// Why the remote host could not serve a call right now, as carried in the
/// `detail` object of a `fed_target_unavailable` `call_frame` error body.
///
/// Without this, every refusal of that code reads the same to a person
/// ("cannot reach your Mac"), even when the host is only starting up or
/// restarting the target module and will serve the same call shortly. The hint
/// lets a caller tell those cases apart.
///
/// Decoding rule (applied by `FedFrame.terminalHostRefusal`, the one place the
/// wire key names are spelled). Both value sets are CLOSED:
/// - The hint is read only when the error `code` is exactly
///   `fed_target_unavailable`. Another code's `detail` using the same key names
///   never produces a hint.
/// - `detail` absent, not an object, `host_refusal` absent, or `host_refusal`
///   outside `HostRefusal`: no hint (`nil`). The call's failure is reported as
///   it would be without the hint; an unknown value is never passed through.
/// - `host_reason` is read only when `host_refusal` is `module_warming`. An
///   unknown `host_reason` is dropped: the hint keeps its `hostRefusal` and
///   `hostReason` is `nil`.
/// - Unknown extra fields in `detail` are ignored.
///
/// The hint is informational only. It never changes how a call settles.
public struct FedHostRefusalHint: Sendable, Equatable, Codable {
    /// The state of the host's target module when it refused the call.
    public enum HostRefusal: String, Sendable, Equatable, Codable, CaseIterable {
        case moduleWarming = "module_warming"
        case moduleReloading = "module_reloading"
        case targetUnavailable = "target_unavailable"
        case moduleTimeout = "module_timeout"
    }

    /// Why a warming module is not ready yet. Only meaningful with
    /// `HostRefusal.moduleWarming`.
    public enum HostReason: String, Sendable, Equatable, Codable, CaseIterable {
        case declaredNotReady = "declared_not_ready"
        case requiredCapabilityUnprovided = "required_capability_unprovided"
    }

    public var hostRefusal: HostRefusal
    public var hostReason: HostReason?

    public init(hostRefusal: HostRefusal, hostReason: HostReason? = nil) {
        self.hostRefusal = hostRefusal
        self.hostReason = hostReason
    }
}
