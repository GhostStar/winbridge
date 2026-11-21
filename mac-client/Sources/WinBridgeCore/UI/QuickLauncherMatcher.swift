import Foundation

enum QuickLauncherItem: Equatable {
    case window(WindowInfo, isSubscribed: Bool)
    case app(AppEntry, isPinned: Bool)
    case desktop(isOpen: Bool)
    case connectHost(host: String, port: UInt16)
    case wakeHost(macAddress: String, host: String)

    var title: String {
        switch self {
        case .window(let info, _):
            return info.title.isEmpty ? info.processName : info.title
        case .app(let app, _):
            return app.name
        case .desktop(let isOpen):
            return isOpen ? "切换到 Windows 完整桌面" : "打开 Windows 完整桌面"
        case .connectHost(let host, let port):
            return "连接 Windows 主机（\(host):\(port)）"
        case .wakeHost(let mac, _):
            return "唤醒并连接 Windows 主机（WoL · \(mac)）"
        }
    }

    var subtitle: String {
        switch self {
        case .window(let info, let isSubscribed):
            let state = isSubscribed ? "已投射 · 切换到前台" : "运行中窗口 · 回车投射"
            let proc = info.processName.isEmpty ? "Windows" : info.processName
            return "\(proc) · \(state)"
        case .app(let app, let isPinned):
            let src = app.source == "Desktop" ? "桌面应用" : "开始菜单应用"
            return isPinned ? "★ 置顶 · \(src) · 回车启动" : "\(src) · 回车启动"
        case .desktop:
            return "主显示器完整桌面（含任务栏与系统托盘）"
        case .connectHost:
            return "当前未连接 · 回车立即建立连接"
        case .wakeHost(_, let host):
            return "发送 Wake-on-LAN 魔术包并等待 \(host) 上线"
        }
    }

    var badgeText: String {
        switch self {
        case .window(_, let isSubscribed):
            return isSubscribed ? "已投射" : "窗口"
        case .app(_, let isPinned):
            return isPinned ? "★ 应用" : "应用"
        case .desktop:
            return "桌面"
        case .connectHost:
            return "连接"
        case .wakeHost:
            return "唤醒"
        }
    }

    var iconPng: Data {
        switch self {
        case .window(let info, _): return info.iconPng
        case .app(let app, _): return app.iconPng
        case .desktop, .connectHost, .wakeHost: return Data()
        }
    }
}

enum QuickLauncherMatcher {
    /// Scores how well `query` matches `primary` and optional `secondary`.
    /// Returns `nil` when not matched, or a positive relevance score (higher is better).
    static func matchScore(query rawQuery: String, primary: String, secondary: String = "") -> Int? {
        let trimmed = rawQuery.trimmingCharacters(in: .whitespacesAndNewlines).lowercased()
        guard !trimmed.isEmpty else { return 0 }

        let tokens = trimmed.split(whereSeparator: \.isWhitespace).map(String.init)
        guard !tokens.isEmpty else { return 0 }

        let primaryLower = primary.lowercased()
        let secondaryLower = secondary.lowercased()

        var totalScore = 0
        for token in tokens {
            let pScore = tokenScore(token: token, target: primaryLower)
            let sScore = secondaryLower.isEmpty ? nil : tokenScore(token: token, target: secondaryLower).map { max(1, $0 - 150) }
            guard let best = [pScore, sScore].compactMap({ $0 }).max() else {
                return nil
            }
            totalScore += best
        }
        return totalScore
    }

    private static func tokenScore(token: String, target: String) -> Int? {
        guard !target.isEmpty else { return nil }
        if target == token {
            return 1200
        }
        if target.hasPrefix(token) {
            return 1000 - min(200, target.count - token.count)
        }
        if let range = target.range(of: token) {
            let dist = target.distance(from: target.startIndex, to: range.lowerBound)
            let boundaryBonus = isWordBoundary(in: target, at: range.lowerBound) ? 120 : 0
            return 720 + boundaryBonus - min(250, dist * 4)
        }

        // Fuzzy subsequence match (e.g. "vsc" -> "Visual Studio Code", "wx" -> "Weixin")
        let tChars = Array(target)
        let qChars = Array(token)
        var tIdx = 0
        var prevMatchIdx = -2
        var score = 320
        var matchedAll = true

        for q in qChars {
            var found = false
            while tIdx < tChars.count {
                if tChars[tIdx] == q {
                    if tIdx == prevMatchIdx + 1 {
                        score += 35 // consecutive run bonus
                    }
                    if tIdx == 0 || isSeparator(tChars[tIdx - 1]) {
                        score += 45 // word-start initials bonus
                    }
                    score -= min(10, max(0, tIdx - max(0, prevMatchIdx) - 1))
                    prevMatchIdx = tIdx
                    tIdx += 1
                    found = true
                    break
                }
                tIdx += 1
            }
            if !found {
                matchedAll = false
                break
            }
        }
        return matchedAll ? max(50, score) : nil
    }

    private static func isSeparator(_ ch: Character) -> Bool {
        ch == " " || ch == "-" || ch == "_" || ch == "." || ch == "/" || ch == "\\" || ch == "(" || ch == "["
    }

    private static func isWordBoundary(in text: String, at index: String.Index) -> Bool {
        if index == text.startIndex { return true }
        let prev = text.index(before: index)
        return isSeparator(text[prev])
    }

    /// Builds ranked Spotlight results prioritizing open Windows windows above installed apps.
    static func search(
        query rawQuery: String,
        isConnected: Bool,
        host: String,
        port: UInt16,
        macAddress: String?,
        windows: [WindowInfo],
        subscribedRootIds: Set<UInt64>,
        apps: [AppEntry],
        pinnedAppIds: Set<String>,
        isDesktopOpen: Bool,
        limit: Int = 40
    ) -> [QuickLauncherItem] {
        let query = rawQuery.trimmingCharacters(in: .whitespacesAndNewlines)

        if !isConnected {
            var offline: [QuickLauncherItem] = []
            if !host.isEmpty {
                offline.append(.connectHost(host: host, port: port))
            }
            if let mac = macAddress, WakeOnLan.normalizeMac(mac) != nil {
                offline.append(.wakeHost(macAddress: WakeOnLan.normalizeMac(mac) ?? mac, host: host))
            }
            if query.isEmpty {
                return offline
            }
            return offline.filter {
                matchScore(query: query, primary: $0.title, secondary: $0.subtitle) != nil
            }
        }

        var scored: [(priority: Int, score: Int, order: Int, item: QuickLauncherItem)] = []
        var order = 0

        // 1. Open Windows windows (highest priority tier)
        for win in windows {
            let isSub = subscribedRootIds.contains(win.windowId)
            let primary = win.title.isEmpty ? win.processName : win.title
            let secondary = "\(win.processName) \(isSub ? "已投射" : "窗口")"
            if query.isEmpty {
                let tier = isSub ? 400 : 300
                scored.append((tier, 0, order, .window(win, isSubscribed: isSub)))
                order += 1
            } else if let s = matchScore(query: query, primary: primary, secondary: secondary) {
                let tier = isSub ? 400 : 350
                scored.append((tier, s, order, .window(win, isSubscribed: isSub)))
                order += 1
            }
        }

        // 2. Full Desktop entry
        let desktopItem = QuickLauncherItem.desktop(isOpen: isDesktopOpen)
        if query.isEmpty {
            scored.append((250, 0, order, desktopItem))
            order += 1
        } else if let s = matchScore(query: query, primary: "Windows 完整桌面 Desktop", secondary: "全屏 任务栏 UAC") {
            scored.append((260, s, order, desktopItem))
            order += 1
        }

        // 3. Installed Windows apps (Start Menu / Desktop)
        for app in apps {
            let isPinned = pinnedAppIds.contains(app.appId)
            if query.isEmpty {
                let tier = isPinned ? 220 : 100
                scored.append((tier, 0, order, .app(app, isPinned: isPinned)))
                order += 1
            } else if let s = matchScore(query: query, primary: app.name, secondary: app.source) {
                let tier = isPinned ? 210 : 200
                scored.append((tier, s, order, .app(app, isPinned: isPinned)))
                order += 1
            }
        }

        scored.sort { a, b in
            if query.isEmpty {
                if a.priority != b.priority { return a.priority > b.priority }
                return a.order < b.order
            }
            // When searching, combine base priority with match score so open windows win ties
            // while an exact app match still beats a weak fuzzy window match.
            let totalA = a.score + a.priority
            let totalB = b.score + b.priority
            if totalA != totalB { return totalA > totalB }
            if a.priority != b.priority { return a.priority > b.priority }
            return a.order < b.order
        }

        return Array(scored.prefix(limit).map(\.item))
    }
}
