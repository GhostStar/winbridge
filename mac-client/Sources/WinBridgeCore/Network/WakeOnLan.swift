import Darwin
import Foundation

enum WakeOnLanError: Error, LocalizedError {
    case invalidMacAddress(String)
    case socketCreateFailed(Int32)
    case broadcastEnableFailed(Int32)
    case sendFailed(Int32)

    var errorDescription: String? {
        switch self {
        case .invalidMacAddress(let raw):
            return "无效的 MAC 地址「\(raw)」（格式应为 AA:BB:CC:DD:EE:FF）"
        case .socketCreateFailed(let code):
            return "创建 UDP 套接字失败（errno=\(code)）"
        case .broadcastEnableFailed(let code):
            return "开启 UDP 广播失败（errno=\(code)）"
        case .sendFailed(let code):
            return "发送 Wake-on-LAN 魔术包失败（errno=\(code)）"
        }
    }
}

enum WakeOnLan {
    static func parseMac(_ raw: String) -> [UInt8]? {
        let hex = raw
            .trimmingCharacters(in: .whitespacesAndNewlines)
            .filter { $0 != ":" && $0 != "-" && $0 != "." && !$0.isWhitespace }
        guard hex.count == 12 else { return nil }
        var bytes: [UInt8] = []
        bytes.reserveCapacity(6)
        var idx = hex.startIndex
        for _ in 0..<6 {
            let next = hex.index(idx, offsetBy: 2)
            guard let byte = UInt8(hex[idx..<next], radix: 16) else { return nil }
            bytes.append(byte)
            idx = next
        }
        if bytes.allSatisfy({ $0 == 0x00 }) || bytes.allSatisfy({ $0 == 0xFF }) {
            return nil
        }
        return bytes
    }

    static func formatMac(_ bytes: [UInt8]) -> String {
        bytes.map { String(format: "%02X", $0) }.joined(separator: ":")
    }

    static func normalizeMac(_ raw: String) -> String? {
        parseMac(raw).map(formatMac)
    }

    /// Builds a standard 102-byte AMD Magic Packet: 6 bytes of 0xFF followed by 16 repetitions of the 6-byte MAC.
    static func buildMagicPacket(mac: [UInt8]) -> Data {
        var data = Data(capacity: 6 + 16 * 6)
        data.append(contentsOf: repeatElement(UInt8(0xFF), count: 6))
        for _ in 0..<16 {
            data.append(contentsOf: mac)
        }
        return data
    }

    /// Computes directed broadcast candidates from an IPv4 hint and active macOS network interfaces,
    /// always including `255.255.255.255`.
    static func targetBroadcastAddresses(hostHint: String?) -> [String] {
        var targets: [String] = []
        func appendUnique(_ addr: String) {
            if !targets.contains(addr) {
                targets.append(addr)
            }
        }

        if let hint = hostHint?.trimmingCharacters(in: .whitespacesAndNewlines),
           !hint.isEmpty {
            let parts = hint.split(separator: ".")
            if parts.count == 4,
               let a = UInt8(parts[0]),
               let b = UInt8(parts[1]),
               let c = UInt8(parts[2]),
               let d = UInt8(parts[3]),
               !(a == 127 || (a == 0 && b == 0 && c == 0 && d == 0)) {
                appendUnique("\(a).\(b).\(c).255")
            }
        }

        for addr in interfaceBroadcastIPv4s() {
            appendUnique(addr)
        }
        appendUnique("255.255.255.255")
        return targets
    }

    private static func interfaceBroadcastIPv4s() -> [String] {
        var ifaddrPtr: UnsafeMutablePointer<ifaddrs>?
        guard getifaddrs(&ifaddrPtr) == 0, let first = ifaddrPtr else {
            return []
        }
        defer { freeifaddrs(ifaddrPtr) }

        var results: [String] = []
        var cur: UnsafeMutablePointer<ifaddrs>? = first
        while let ifa = cur?.pointee {
            let flags = Int32(ifa.ifa_flags)
            let isUp = (flags & IFF_UP) != 0
            let isLoopback = (flags & IFF_LOOPBACK) != 0
            let supportsBroadcast = (flags & IFF_BROADCAST) != 0
            if isUp && !isLoopback && supportsBroadcast,
               let dst = ifa.ifa_dstaddr,
               dst.pointee.sa_family == sa_family_t(AF_INET) {
                var sin = dst.withMemoryRebound(to: sockaddr_in.self, capacity: 1) { $0.pointee }
                var buf = [CChar](repeating: 0, count: Int(INET_ADDRSTRLEN))
                if inet_ntop(AF_INET, &sin.sin_addr, &buf, socklen_t(INET_ADDRSTRLEN)) != nil {
                    let ip = String(cString: buf)
                    if !ip.isEmpty && ip != "0.0.0.0" && !results.contains(ip) {
                        results.append(ip)
                    }
                }
            }
            cur = ifa.ifa_next
        }
        return results
    }

    @discardableResult
    static func send(macAddress: String, hostHint: String? = nil) throws -> [String] {
        guard let macBytes = parseMac(macAddress) else {
            throw WakeOnLanError.invalidMacAddress(macAddress)
        }
        let packet = buildMagicPacket(mac: macBytes)
        let fd = socket(AF_INET, SOCK_DGRAM, IPPROTO_UDP)
        guard fd >= 0 else {
            throw WakeOnLanError.socketCreateFailed(errno)
        }
        defer { Darwin.close(fd) }

        var enable: Int32 = 1
        guard setsockopt(fd, SOL_SOCKET, SO_BROADCAST, &enable, socklen_t(MemoryLayout<Int32>.size)) == 0 else {
            throw WakeOnLanError.broadcastEnableFailed(errno)
        }

        let targets = targetBroadcastAddresses(hostHint: hostHint)
        let ports: [UInt16] = [9, 7]
        var succeededTargets: [String] = []
        var lastErr: Int32 = 0

        for addrStr in targets {
            var inAddr = in_addr()
            guard addrStr.withCString({ inet_pton(AF_INET, $0, &inAddr) }) == 1 else {
                continue
            }
            var addrSent = false
            for port in ports {
                var sin = sockaddr_in()
                sin.sin_len = UInt8(MemoryLayout<sockaddr_in>.size)
                sin.sin_family = sa_family_t(AF_INET)
                sin.sin_port = port.bigEndian
                sin.sin_addr = inAddr

                let sent = packet.withUnsafeBytes { rawBuf -> ssize_t in
                    withUnsafePointer(to: &sin) { sinPtr in
                        sinPtr.withMemoryRebound(to: sockaddr.self, capacity: 1) { saPtr in
                            sendto(
                                fd,
                                rawBuf.baseAddress,
                                packet.count,
                                0,
                                saPtr,
                                socklen_t(MemoryLayout<sockaddr_in>.size)
                            )
                        }
                    }
                }
                if sent == packet.count {
                    addrSent = true
                } else {
                    lastErr = errno
                }
            }
            if addrSent {
                succeededTargets.append(addrStr)
            }
        }

        guard !succeededTargets.isEmpty else {
            throw WakeOnLanError.sendFailed(lastErr)
        }
        return succeededTargets
    }
}
