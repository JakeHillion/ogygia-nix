import Foundation

/// The line protocol spoken by ogygia-macos-nebulad, mirroring
/// src/ogygia-macos-nebulad/src/protocol.rs: one JSON object per line each
/// way over the daemon's Unix socket.
enum DaemonProtocol {
    static let socketPath = "/var/run/ogygia/macos-nebulad.sock"

    enum Request: Encodable, Sendable {
        case status
        case refresh
        case setSettings(repositoryURL: String, branch: String, device: String)

        private enum CodingKeys: String, CodingKey {
            case command, repositoryURL = "repository_url", branch, device
        }

        func encode(to encoder: any Encoder) throws {
            var container = encoder.container(keyedBy: CodingKeys.self)
            switch self {
            case .status:
                try container.encode("status", forKey: .command)
            case .refresh:
                try container.encode("refresh", forKey: .command)
            case .setSettings(let repositoryURL, let branch, let device):
                try container.encode("set_settings", forKey: .command)
                try container.encode(repositoryURL, forKey: .repositoryURL)
                try container.encode(branch, forKey: .branch)
                try container.encode(device, forKey: .device)
            }
        }
    }

    /// `{"status": {...}}` or `{"error": "..."}`.
    struct Response: Decodable, Sendable {
        var status: Status?
        var error: String?
    }

    struct Status: Decodable, Sendable, Equatable {
        var build: String?
        var repositoryURL: String?
        var branch: String?
        var device: String?
        var settingsManaged: Bool
        var publicKey: String?
        var address: String?
        var certExpires: Date?
        var updating: Bool
        var lastUpdate: Date?
        var updateError: String?
        var nebula: Nebula

        private enum CodingKeys: String, CodingKey {
            case build
            case repositoryURL = "repository_url"
            case branch
            case device
            case settingsManaged = "settings_managed"
            case publicKey = "public_key"
            case address
            case certExpires = "cert_expires"
            case updating
            case lastUpdate = "last_update"
            case updateError = "update_error"
            case nebula
        }
    }

    enum Nebula: Decodable, Sendable, Equatable {
        case stopped
        case running(since: Date)
        case exited(reason: String)

        private enum CodingKeys: String, CodingKey {
            case state, since, reason
        }

        init(from decoder: any Decoder) throws {
            let container = try decoder.container(keyedBy: CodingKeys.self)
            switch try container.decode(String.self, forKey: .state) {
            case "running":
                self = .running(since: try container.decode(Date.self, forKey: .since))
            case "exited":
                self = .exited(reason: try container.decode(String.self, forKey: .reason))
            default:
                self = .stopped
            }
        }
    }

    static func encode(_ request: Request) throws -> Data {
        var line = try JSONEncoder().encode(request)
        line.append(UInt8(ascii: "\n"))
        return line
    }

    static func decode(_ line: Data) throws -> Response {
        let decoder = JSONDecoder()
        // chrono writes RFC 3339 with fractional seconds when it has them.
        decoder.dateDecodingStrategy = .custom { decoder in
            let text = try decoder.singleValueContainer().decode(String.self)
            let formatter = ISO8601DateFormatter()
            formatter.formatOptions = [.withInternetDateTime, .withFractionalSeconds]
            if let date = formatter.date(from: text) {
                return date
            }
            formatter.formatOptions = [.withInternetDateTime]
            if let date = formatter.date(from: text) {
                return date
            }
            throw DecodingError.dataCorrupted(
                .init(codingPath: decoder.codingPath, debugDescription: "not a date: \(text)"))
        }
        return try decoder.decode(Response.self, from: line)
    }
}
