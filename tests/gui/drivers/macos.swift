// GUI test driver for macOS: posts real input events (CGEvent) and uses the Accessibility
// API for window resizing/zooming. Built by tests/gui/lib.sh with swiftc; one command per run:
//
//   driver check                 exit 0 if this process may post events (Accessibility)
//   driver move X Y [left]       move the pointer (global coordinates in points); with
//                                "left", as a drag with the left button held
//   driver down|up|click BUTTON  press / release / click left or right at the pointer
//   driver wheel N               N wheel lines (positive scrolls down)
//   driver key COMBO             e.g. cmd+shift+t, Return, alt+Left, shift+F10
//   driver type TEXT...          type text (as Unicode key events)
//   driver activate PID          bring the app to the front
//   driver resize PID W H        resize its focused window (points)
//   driver maximize PID          zoom its focused window (Option-click on the green button)
//
// Test tooling only: not part of BannedBrowser and not built by cargo.
import AppKit
import ApplicationServices
import Foundation

struct DriverError: Error, CustomStringConvertible {
    let description: String
}

func post(_ event: CGEvent?) throws {
    guard let event = event else { throw DriverError(description: "could not create event") }
    event.post(tap: .cghidEventTap)
    usleep(15_000)
}

func pointer() -> CGPoint {
    CGEvent(source: nil)?.location ?? .zero
}

func mouse(_ type: CGEventType, _ at: CGPoint, _ button: CGMouseButton, clicks: Int64 = 1) throws {
    let e = CGEvent(mouseEventSource: nil, mouseType: type, mouseCursorPosition: at, mouseButton: button)
    e?.setIntegerValueField(.mouseEventClickState, value: clicks)
    try post(e)
}

func button(_ name: String) throws -> (CGMouseButton, CGEventType, CGEventType) {
    switch name {
    case "left": return (.left, .leftMouseDown, .leftMouseUp)
    case "right": return (.right, .rightMouseDown, .rightMouseUp)
    default: throw DriverError(description: "unknown button \(name)")
    }
}

// US ANSI virtual key codes (HIToolbox Events.h).
let keyCodes: [String: CGKeyCode] = [
    "a": 0, "s": 1, "d": 2, "f": 3, "h": 4, "g": 5, "z": 6, "x": 7, "c": 8, "v": 9, "b": 11,
    "q": 12, "w": 13, "e": 14, "r": 15, "y": 16, "t": 17, "1": 18, "2": 19, "3": 20, "4": 21,
    "6": 22, "5": 23, "=": 24, "9": 25, "7": 26, "-": 27, "8": 28, "0": 29, "]": 30, "o": 31,
    "u": 32, "[": 33, "i": 34, "p": 35, "l": 37, "j": 38, "k": 40, ";": 41, ",": 43, "/": 44,
    "n": 45, "m": 46, ".": 47,
    "Return": 36, "Tab": 48, "space": 49, "BackSpace": 51, "Escape": 53, "Delete": 117,
    "Home": 115, "End": 119, "Page_Up": 116, "Page_Down": 121,
    "Left": 123, "Right": 124, "Down": 125, "Up": 126,
    "F1": 122, "F2": 120, "F3": 99, "F4": 118, "F5": 96, "F6": 97, "F7": 98, "F8": 100,
    "F9": 101, "F10": 109, "F11": 103, "F12": 111,
]

let modifiers: [String: (CGKeyCode, CGEventFlags)] = [
    "cmd": (55, .maskCommand), "super": (55, .maskCommand), "shift": (56, .maskShift),
    "alt": (58, .maskAlternate), "ctrl": (59, .maskControl),
]

func key(_ combo: String) throws {
    var parts = combo.split(separator: "+").map(String.init)
    guard let main = parts.popLast() else { throw DriverError(description: "empty key") }
    var flags: CGEventFlags = []
    var mods: [(CGKeyCode, CGEventFlags)] = []
    for p in parts {
        guard let m = modifiers[p] else { throw DriverError(description: "unknown modifier \(p)") }
        mods.append(m)
    }
    // Press the modifiers as real keys (apps track them through flags-changed events).
    for (code, flag) in mods {
        flags.insert(flag)
        let e = CGEvent(keyboardEventSource: nil, virtualKey: code, keyDown: true)
        e?.flags = flags
        try post(e)
    }
    guard let code = keyCodes[main] ?? keyCodes[main.lowercased()] else {
        throw DriverError(description: "unknown key \(main)")
    }
    for down in [true, false] {
        let e = CGEvent(keyboardEventSource: nil, virtualKey: code, keyDown: down)
        e?.flags = flags
        try post(e)
    }
    for (code, flag) in mods.reversed() {
        flags.remove(flag)
        let e = CGEvent(keyboardEventSource: nil, virtualKey: code, keyDown: false)
        e?.flags = flags
        try post(e)
    }
}

func type(_ text: String) throws {
    for ch in text {
        let units = Array(String(ch).utf16)
        for down in [true, false] {
            let e = CGEvent(keyboardEventSource: nil, virtualKey: 0, keyDown: down)
            e?.keyboardSetUnicodeString(stringLength: units.count, unicodeString: units)
            try post(e)
        }
    }
}

func focusedWindow(_ pid: pid_t) throws -> AXUIElement {
    let app = AXUIElementCreateApplication(pid)
    for _ in 0..<50 {
        var value: CFTypeRef?
        if AXUIElementCopyAttributeValue(app, kAXFocusedWindowAttribute as CFString, &value) == .success,
           let v = value {
            return v as! AXUIElement
        }
        var windows: CFTypeRef?
        if AXUIElementCopyAttributeValue(app, kAXWindowsAttribute as CFString, &windows) == .success,
           let list = windows as? [AXUIElement], let first = list.first {
            return first
        }
        usleep(100_000)
    }
    throw DriverError(description: "no window for pid \(pid) (is Accessibility granted?)")
}

func run(_ args: [String]) throws {
    guard let cmd = args.first else { throw DriverError(description: "no command") }
    func int(_ i: Int) throws -> Int {
        guard i < args.count, let v = Int(args[i]) else { throw DriverError(description: "bad argument \(i)") }
        return v
    }
    switch cmd {
    case "check":
        if !AXIsProcessTrusted() { throw DriverError(description: "not trusted for Accessibility") }
    case "move":
        let p = CGPoint(x: try int(1), y: try int(2))
        let drag = args.count > 3 && args[3] == "left"
        try mouse(drag ? .leftMouseDragged : .mouseMoved, p, .left)
    case "down":
        let (b, down, _) = try button(args[1])
        try mouse(down, pointer(), b)
    case "up":
        let (b, _, up) = try button(args[1])
        try mouse(up, pointer(), b)
    case "click":
        let (b, down, up) = try button(args[1])
        let p = pointer()
        try mouse(down, p, b)
        try mouse(up, p, b)
    case "wheel":
        let n = try int(1)
        for _ in 0..<abs(n) {
            let e = CGEvent(scrollWheelEvent2Source: nil, units: .line, wheelCount: 1,
                            wheel1: Int32(n > 0 ? -3 : 3), wheel2: 0, wheel3: 0)
            try post(e)
        }
    case "key":
        try key(args[1])
    case "type":
        try type(args.dropFirst().joined(separator: " "))
    case "activate":
        let pid = pid_t(try int(1))
        guard let app = NSRunningApplication(processIdentifier: pid) else {
            throw DriverError(description: "no app with pid \(pid)")
        }
        app.activate(options: [.activateAllWindows])
        usleep(300_000)
    case "resize":
        let w = try focusedWindow(pid_t(try int(1)))
        var size = CGSize(width: try int(2), height: try int(3))
        guard let v = AXValueCreate(.cgSize, &size),
              AXUIElementSetAttributeValue(w, kAXSizeAttribute as CFString, v) == .success else {
            throw DriverError(description: "resize failed")
        }
    case "maximize":
        // Pressing the green button means full screen on current macOS; a user zooms a window
        // with Option-click on it. Do exactly that, with real events at the button's position.
        let w = try focusedWindow(pid_t(try int(1)))
        var zoom: CFTypeRef?
        guard AXUIElementCopyAttributeValue(w, kAXZoomButtonAttribute as CFString, &zoom) == .success,
              let button = zoom
        else {
            throw DriverError(description: "no zoom button")
        }
        var posRef: CFTypeRef?
        var sizeRef: CFTypeRef?
        var origin = CGPoint.zero
        var size = CGSize.zero
        guard AXUIElementCopyAttributeValue(button as! AXUIElement, kAXPositionAttribute as CFString, &posRef) == .success,
              AXUIElementCopyAttributeValue(button as! AXUIElement, kAXSizeAttribute as CFString, &sizeRef) == .success,
              let p = posRef, let sz = sizeRef,
              AXValueGetValue(p as! AXValue, .cgPoint, &origin),
              AXValueGetValue(sz as! AXValue, .cgSize, &size)
        else {
            throw DriverError(description: "no zoom button frame")
        }
        let center = CGPoint(x: origin.x + size.width / 2, y: origin.y + size.height / 2)
        try mouse(.mouseMoved, center, .left)
        let option = CGEvent(keyboardEventSource: nil, virtualKey: 58, keyDown: true)
        option?.flags = .maskAlternate
        try post(option)
        for type in [CGEventType.leftMouseDown, .leftMouseUp] {
            let e = CGEvent(mouseEventSource: nil, mouseType: type, mouseCursorPosition: center, mouseButton: .left)
            e?.flags = .maskAlternate
            e?.setIntegerValueField(.mouseEventClickState, value: 1)
            try post(e)
        }
        let release = CGEvent(keyboardEventSource: nil, virtualKey: 58, keyDown: false)
        release?.flags = []
        try post(release)
    default:
        throw DriverError(description: "unknown command \(cmd)")
    }
}

do {
    try run(Array(CommandLine.arguments.dropFirst()))
} catch {
    FileHandle.standardError.write("driver: \(error)\n".data(using: .utf8)!)
    exit(1)
}
