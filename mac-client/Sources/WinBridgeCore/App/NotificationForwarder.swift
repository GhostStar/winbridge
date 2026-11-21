import Foundation
import UserNotifications

struct NotificationBannerContent: Equatable, Sendable {
    let title: String
    let subtitle: String
    let body: String
}

/// Forwards Windows Action Center toast notifications into macOS Notification Center.
/// Strictly guards `UNUserNotificationCenter.current()` to real `.app` bundles so `swift test`
/// and standalone CLI runs never trigger `NSInternalInconsistencyException`.
final class NotificationForwarder: NSObject, UNUserNotificationCenterDelegate, @unchecked Sendable {
    var onNotificationClicked: (@MainActor (UInt64, String, String) -> Void)?

    private var didConfigureCenter = false
    private var didRequestAuth = false

    static var isBundledApp: Bool {
        Bundle.main.bundleURL.pathExtension.lowercased() == "app"
            && Bundle.main.bundleIdentifier != nil
    }

    static func formatBanner(from note: HostNotification) -> NotificationBannerContent {
        let app = note.appName.trimmingCharacters(in: .whitespacesAndNewlines)
        let title = note.title.trimmingCharacters(in: .whitespacesAndNewlines)
        let body = note.body.trimmingCharacters(in: .whitespacesAndNewlines)
        let heading = app.isEmpty ? "Windows 通知" : app

        if !title.isEmpty && !body.isEmpty {
            return NotificationBannerContent(title: heading, subtitle: title, body: body)
        } else if !title.isEmpty {
            return NotificationBannerContent(title: heading, subtitle: "", body: title)
        } else {
            return NotificationBannerContent(title: heading, subtitle: "", body: body)
        }
    }

    static func selectTargetWindow(
        windowId: UInt64,
        processName: String,
        appName: String,
        availableWindows: [WindowInfo]
    ) -> WindowInfo? {
        if windowId != 0, let exactId = availableWindows.first(where: { $0.windowId == windowId }) {
            return exactId
        }
        let procLower = processName.trimmingCharacters(in: .whitespacesAndNewlines).lowercased()
        let procStem = procLower.hasSuffix(".exe") ? String(procLower.dropLast(4)) : procLower
        let appLower = appName.trimmingCharacters(in: .whitespacesAndNewlines).lowercased()

        if !procLower.isEmpty,
           let exactProc = availableWindows.first(where: {
               $0.kind != .popup && $0.processName.lowercased() == procLower
           }) {
            return exactProc
        }
        if procStem.count >= 3,
           let stemMatch = availableWindows.first(where: { win in
               guard win.kind != .popup else { return false }
               let wProc = win.processName.lowercased()
               let wStem = wProc.hasSuffix(".exe") ? String(wProc.dropLast(4)) : wProc
               return !wStem.isEmpty && (procStem.contains(wStem) || wStem.contains(procStem))
           }) {
            return stemMatch
        }
        if appLower.count >= 2,
           let titleMatch = availableWindows.first(where: {
               $0.kind != .popup && $0.title.lowercased().contains(appLower)
           }) {
            return titleMatch
        }
        return nil
    }

    @MainActor
    func prepareIfBundled() {
        guard Self.isBundledApp, !didConfigureCenter else { return }
        didConfigureCenter = true
        UNUserNotificationCenter.current().delegate = self
    }

    @MainActor
    func requestAuthorizationIfNeeded() {
        guard Self.isBundledApp else { return }
        prepareIfBundled()
        guard !didRequestAuth else { return }
        didRequestAuth = true
        UNUserNotificationCenter.current().requestAuthorization(options: [.alert, .sound, .badge]) { granted, error in
            if let error {
                ClientLogger.shared.error("UNUserNotificationCenter auth error: \(error.localizedDescription)")
            } else {
                ClientLogger.shared.info("UNUserNotificationCenter authorization granted=\(granted)")
            }
        }
    }

    @MainActor
    func post(_ note: HostNotification) {
        guard Self.isBundledApp else { return }
        prepareIfBundled()
        let formatted = Self.formatBanner(from: note)
        let content = UNMutableNotificationContent()
        content.title = formatted.title
        content.subtitle = formatted.subtitle
        content.body = formatted.body
        content.sound = .default
        content.userInfo = [
            "windowId": NSNumber(value: note.windowId),
            "processName": note.processName,
            "appName": note.appName,
        ]
        let identifier = note.notificationId != 0
            ? "winbridge-toast-\(note.notificationId)"
            : "winbridge-toast-\(UUID().uuidString)"
        let request = UNNotificationRequest(identifier: identifier, content: content, trigger: nil)
        UNUserNotificationCenter.current().add(request) { error in
            if let error {
                ClientLogger.shared.error("Failed to post macOS notification: \(error.localizedDescription)")
            }
        }
    }

    // Show native macOS banner + sound even while WinBridge is in the foreground.
    func userNotificationCenter(
        _ center: UNUserNotificationCenter,
        willPresent notification: UNNotification,
        withCompletionHandler completionHandler: @escaping (UNNotificationPresentationOptions) -> Void
    ) {
        completionHandler([.banner, .sound, .list])
    }

    func userNotificationCenter(
        _ center: UNUserNotificationCenter,
        didReceive response: UNNotificationResponse,
        withCompletionHandler completionHandler: @escaping () -> Void
    ) {
        let info = response.notification.request.content.userInfo
        let windowId = (info["windowId"] as? NSNumber)?.uint64Value ?? 0
        let processName = (info["processName"] as? String) ?? ""
        let appName = (info["appName"] as? String) ?? ""
        let callback = onNotificationClicked
        Task { @MainActor in
            callback?(windowId, processName, appName)
        }
        completionHandler()
    }
}
