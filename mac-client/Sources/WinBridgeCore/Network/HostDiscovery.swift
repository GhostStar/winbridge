import Darwin
import Foundation

struct DiscoveredHost: Equatable, Hashable, Sendable {
    let name: String
    let host: String
    let port: UInt16
    let tlsEnabled: Bool
    let shortFingerprint: String?

    var displayLabel: String {
        let lock = tlsEnabled ? "🔒 " : ""
        return "\(lock)\(name) (\(host):\(port))"
    }

    static func parseTXTRecord(_ data: Data?) -> (tls: Bool, shortFingerprint: String?) {
        guard let data else { return (true, nil) }
        let dict = NetService.dictionary(fromTXTRecord: data)
        let tlsStr = dict["tls"].flatMap { String(data: $0, encoding: .utf8) }
        let tls = (tlsStr != "0")
        let fp = dict["fp"].flatMap { String(data: $0, encoding: .utf8) }
        return (tls, fp?.isEmpty == false ? fp : nil)
    }
}

/// Discovers WinBridge Windows hosts advertising `_winbridge._tcp.` on the local network
/// via Apple's native Bonjour (`NetServiceBrowser`) with zero third-party dependencies.
@MainActor
final class HostDiscovery: NSObject, NetServiceBrowserDelegate, NetServiceDelegate {
    private let browser = NetServiceBrowser()
    private var resolving: [NetService] = []
    private(set) var hosts: [DiscoveredHost] = []
    var onHostsChanged: (([DiscoveredHost]) -> Void)?

    override init() {
        super.init()
        browser.delegate = self
    }

    func start() {
        browser.searchForServices(ofType: "_winbridge._tcp.", inDomain: "local.")
    }

    func stop() {
        browser.stop()
        for svc in resolving {
            svc.stop()
        }
        resolving.removeAll()
    }

    nonisolated func netServiceBrowser(
        _ browser: NetServiceBrowser,
        didFind service: NetService,
        moreComing: Bool
    ) {
        MainActor.assumeIsolated {
            service.delegate = self
            resolving.append(service)
            service.resolve(withTimeout: 5.0)
        }
    }

    nonisolated func netServiceBrowser(
        _ browser: NetServiceBrowser,
        didRemove service: NetService,
        moreComing: Bool
    ) {
        MainActor.assumeIsolated {
            resolving.removeAll { $0 === service }
            hosts.removeAll { $0.name == service.name }
            onHostsChanged?(hosts)
        }
    }

    nonisolated func netServiceDidResolveAddress(_ sender: NetService) {
        MainActor.assumeIsolated {
            guard sender.port > 0 && sender.port <= 65535 else { return }
            let ip = Self.pickBestIP(from: sender.addresses ?? [])
                ?? sender.hostName?.trimmingCharacters(in: CharacterSet(charactersIn: "."))
                ?? ""
            guard !ip.isEmpty else { return }
            let (tls, fp) = DiscoveredHost.parseTXTRecord(sender.txtRecordData())
            let entry = DiscoveredHost(
                name: sender.name,
                host: ip,
                port: UInt16(sender.port),
                tlsEnabled: tls,
                shortFingerprint: fp
            )
            hosts.removeAll { $0.name == entry.name }
            hosts.append(entry)
            hosts.sort { $0.name.localizedCaseInsensitiveCompare($1.name) == .orderedAscending }
            onHostsChanged?(hosts)
        }
    }

    private static func pickBestIP(from addresses: [Data]) -> String? {
        var fallbackIPv4: String?
        var fallbackIPv6: String?
        for data in addresses {
            let result: (family: Int32, ip: String)? = data.withUnsafeBytes { raw in
                guard let base = raw.baseAddress, raw.count >= MemoryLayout<sockaddr>.size else {
                    return nil
                }
                let sa = base.assumingMemoryBound(to: sockaddr.self).pointee
                if sa.sa_family == sa_family_t(AF_INET), raw.count >= MemoryLayout<sockaddr_in>.size {
                    var addr = base.assumingMemoryBound(to: sockaddr_in.self).pointee.sin_addr
                    var buf = [CChar](repeating: 0, count: Int(INET_ADDRSTRLEN))
                    guard inet_ntop(AF_INET, &addr, &buf, socklen_t(INET_ADDRSTRLEN)) != nil else {
                        return nil
                    }
                    return (AF_INET, String(cString: buf))
                } else if sa.sa_family == sa_family_t(AF_INET6), raw.count >= MemoryLayout<sockaddr_in6>.size {
                    var addr = base.assumingMemoryBound(to: sockaddr_in6.self).pointee.sin6_addr
                    var buf = [CChar](repeating: 0, count: Int(INET6_ADDRSTRLEN))
                    guard inet_ntop(AF_INET6, &addr, &buf, socklen_t(INET6_ADDRSTRLEN)) != nil else {
                        return nil
                    }
                    return (AF_INET6, String(cString: buf))
                }
                return nil
            }
            guard let (family, ip) = result else { continue }
            if family == AF_INET {
                if !ip.hasPrefix("169.254.") && !ip.hasPrefix("127.") {
                    return ip
                }
                fallbackIPv4 = fallbackIPv4 ?? ip
            } else if family == AF_INET6 && !ip.hasPrefix("fe80:") && ip != "::1" {
                fallbackIPv6 = fallbackIPv6 ?? ip
            }
        }
        return fallbackIPv4 ?? fallbackIPv6
    }
}
