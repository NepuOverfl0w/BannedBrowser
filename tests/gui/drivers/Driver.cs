// GUI test driver for Windows: sends real OS input with SendInput, like a user would.
// Built by tests/gui/lib.sh with the .NET Framework's csc.exe (C# 5), one command per run:
//
//   driver move X Y              move the pointer (screen coordinates, physical pixels)
//   driver down|up|click BUTTON  press / release / click left or right
//   driver wheel N               N wheel notches (positive scrolls down)
//   driver key COMBO             e.g. ctrl+shift+t, Return, alt+Left, shift+F10
//   driver type TEXT...          type text (as Unicode key events)
//   driver activate PID          bring the process's main window to the front
//   driver resize PID W H        resize its main window (outer size, physical pixels)
//   driver maximize PID          maximize its main window
//   driver shot FILE.png         screenshot of the whole desktop
//
// Test tooling only: not part of BannedBrowser and not built by cargo.
using System;
using System.Collections.Generic;
using System.Diagnostics;
using System.Drawing;
using System.Drawing.Imaging;
using System.Runtime.InteropServices;
using System.Threading;

static class Driver
{
    [StructLayout(LayoutKind.Sequential)]
    struct MOUSEINPUT { public int dx; public int dy; public uint mouseData; public uint dwFlags; public uint time; public IntPtr dwExtraInfo; }

    [StructLayout(LayoutKind.Sequential)]
    struct KEYBDINPUT { public ushort wVk; public ushort wScan; public uint dwFlags; public uint time; public IntPtr dwExtraInfo; }

    [StructLayout(LayoutKind.Explicit)]
    struct InputUnion { [FieldOffset(0)] public MOUSEINPUT mi; [FieldOffset(0)] public KEYBDINPUT ki; }

    [StructLayout(LayoutKind.Sequential)]
    struct INPUT { public uint type; public InputUnion u; }

    [DllImport("user32.dll", SetLastError = true)] static extern uint SendInput(uint n, INPUT[] inputs, int size);
    [DllImport("user32.dll")] static extern bool SetCursorPos(int x, int y);
    [DllImport("user32.dll")] static extern int GetSystemMetrics(int index);
    [DllImport("user32.dll")] static extern uint MapVirtualKey(uint code, uint mapType);
    [DllImport("user32.dll")] static extern bool SetForegroundWindow(IntPtr hwnd);
    [DllImport("user32.dll")] static extern bool BringWindowToTop(IntPtr hwnd);
    [DllImport("user32.dll")] static extern bool ShowWindow(IntPtr hwnd, int cmd);
    [DllImport("user32.dll")] static extern bool SetWindowPos(IntPtr hwnd, IntPtr after, int x, int y, int w, int h, uint flags);
    [DllImport("user32.dll")] static extern bool SetProcessDPIAware();
    delegate bool EnumWindowsProc(IntPtr hwnd, IntPtr lParam);
    [DllImport("user32.dll")] static extern bool EnumWindows(EnumWindowsProc proc, IntPtr lParam);
    [DllImport("user32.dll")] static extern uint GetWindowThreadProcessId(IntPtr hwnd, out uint pid);
    [DllImport("user32.dll")] static extern bool IsWindowVisible(IntPtr hwnd);
    [DllImport("user32.dll")] static extern IntPtr GetWindow(IntPtr hwnd, uint cmd);
    [DllImport("user32.dll")] static extern IntPtr GetForegroundWindow();

    const uint INPUT_MOUSE = 0, INPUT_KEYBOARD = 1;
    const uint MOUSEEVENTF_MOVE = 0x1, MOUSEEVENTF_LEFTDOWN = 0x2, MOUSEEVENTF_LEFTUP = 0x4;
    const uint MOUSEEVENTF_RIGHTDOWN = 0x8, MOUSEEVENTF_RIGHTUP = 0x10, MOUSEEVENTF_WHEEL = 0x800;
    const uint MOUSEEVENTF_VIRTUALDESK = 0x4000, MOUSEEVENTF_ABSOLUTE = 0x8000;
    const uint KEYEVENTF_EXTENDEDKEY = 0x1, KEYEVENTF_KEYUP = 0x2, KEYEVENTF_UNICODE = 0x4;

    static void Send(params INPUT[] inputs)
    {
        uint sent = SendInput((uint)inputs.Length, inputs, Marshal.SizeOf(typeof(INPUT)));
        if (sent != inputs.Length)
            throw new Exception("SendInput sent " + sent + " of " + inputs.Length + " (error " + Marshal.GetLastWin32Error() + ")");
    }

    static INPUT Mouse(uint flags, int dx, int dy, uint data)
    {
        INPUT i = new INPUT();
        i.type = INPUT_MOUSE;
        i.u.mi.dx = dx; i.u.mi.dy = dy; i.u.mi.mouseData = data; i.u.mi.dwFlags = flags;
        return i;
    }

    static INPUT Key(ushort vk, ushort scan, uint flags)
    {
        INPUT i = new INPUT();
        i.type = INPUT_KEYBOARD;
        i.u.ki.wVk = vk; i.u.ki.wScan = scan; i.u.ki.dwFlags = flags;
        return i;
    }

    static void Move(int x, int y)
    {
        SetCursorPos(x, y);
        // Also a real move event, normalized to the virtual desktop (0..65535).
        int vx = GetSystemMetrics(76), vy = GetSystemMetrics(77), vw = GetSystemMetrics(78), vh = GetSystemMetrics(79);
        int nx = (int)Math.Round((x - vx) * 65535.0 / Math.Max(1, vw - 1));
        int ny = (int)Math.Round((y - vy) * 65535.0 / Math.Max(1, vh - 1));
        Send(Mouse(MOUSEEVENTF_MOVE | MOUSEEVENTF_ABSOLUTE | MOUSEEVENTF_VIRTUALDESK, nx, ny, 0));
    }

    static uint ButtonFlag(string button, bool down)
    {
        if (button == "left") return down ? MOUSEEVENTF_LEFTDOWN : MOUSEEVENTF_LEFTUP;
        if (button == "right") return down ? MOUSEEVENTF_RIGHTDOWN : MOUSEEVENTF_RIGHTUP;
        throw new Exception("unknown button " + button);
    }

    static readonly Dictionary<string, ushort> Named = new Dictionary<string, ushort>(StringComparer.OrdinalIgnoreCase)
    {
        {"ctrl", 0x11}, {"shift", 0x10}, {"alt", 0x12}, {"super", 0x5B},
        {"Return", 0x0D}, {"Escape", 0x1B}, {"Tab", 0x09}, {"BackSpace", 0x08}, {"space", 0x20},
        {"Left", 0x25}, {"Up", 0x26}, {"Right", 0x27}, {"Down", 0x28},
        {"Home", 0x24}, {"End", 0x23}, {"Page_Up", 0x21}, {"Page_Down", 0x22},
        {"Delete", 0x2E}, {"Insert", 0x2D}, {"Menu", 0x5D},
    };

    // Keys that need KEYEVENTF_EXTENDEDKEY (otherwise they read as the numeric keypad's).
    static readonly HashSet<ushort> Extended = new HashSet<ushort> { 0x21, 0x22, 0x23, 0x24, 0x25, 0x26, 0x27, 0x28, 0x2D, 0x2E, 0x5B, 0x5D };

    static ushort VirtualKey(string name)
    {
        ushort vk;
        if (Named.TryGetValue(name, out vk)) return vk;
        if (name.Length > 1 && (name[0] == 'F' || name[0] == 'f'))
        {
            int n;
            if (int.TryParse(name.Substring(1), out n) && n >= 1 && n <= 24) return (ushort)(0x70 + n - 1);
        }
        if (name.Length == 1)
        {
            char c = char.ToUpperInvariant(name[0]);
            if ((c >= 'A' && c <= 'Z') || (c >= '0' && c <= '9')) return c;
            if (c == '[') return 0xDB;
            if (c == ']') return 0xDD;
        }
        throw new Exception("unknown key " + name);
    }

    static INPUT KeyEvent(ushort vk, bool down)
    {
        ushort scan = (ushort)MapVirtualKey(vk, 0);
        uint flags = (down ? 0 : KEYEVENTF_KEYUP) | (Extended.Contains(vk) ? KEYEVENTF_EXTENDEDKEY : 0);
        return Key(vk, scan, flags);
    }

    static void KeyCombo(string combo)
    {
        string[] parts = combo.Split('+');
        var vks = new List<ushort>();
        foreach (string p in parts) vks.Add(VirtualKey(p));
        var events = new List<INPUT>();
        foreach (ushort vk in vks) events.Add(KeyEvent(vk, true));
        for (int i = vks.Count - 1; i >= 0; i--) events.Add(KeyEvent(vks[i], false));
        Send(events.ToArray());
    }

    static void TypeText(string text)
    {
        foreach (char c in text)
        {
            Send(Key(0, c, KEYEVENTF_UNICODE), Key(0, c, KEYEVENTF_UNICODE | KEYEVENTF_KEYUP));
            Thread.Sleep(10);
        }
    }

    // The process's window to act on: its foreground window if it has one, else its first
    // visible, unowned top-level window. (Process.MainWindowHandle proved unreliable.)
    static IntPtr MainWindow(int pid)
    {
        for (int i = 0; i < 50; i++)
        {
            IntPtr fg = GetForegroundWindow();
            uint fgPid;
            GetWindowThreadProcessId(fg, out fgPid);
            if (fg != IntPtr.Zero && fgPid == (uint)pid) return fg;
            IntPtr found = IntPtr.Zero;
            EnumWindows(delegate (IntPtr hwnd, IntPtr lParam)
            {
                uint wp;
                GetWindowThreadProcessId(hwnd, out wp);
                if (wp == (uint)pid && IsWindowVisible(hwnd) && GetWindow(hwnd, 4 /* GW_OWNER */) == IntPtr.Zero)
                {
                    found = hwnd;
                    return false;
                }
                return true;
            }, IntPtr.Zero);
            if (found != IntPtr.Zero) return found;
            Thread.Sleep(100);
        }
        throw new Exception("process " + pid + " has no visible top-level window");
    }

    static void Activate(int pid)
    {
        IntPtr hwnd = MainWindow(pid);
        // Windows lets the process that sent the last input change the foreground window;
        // tapping Alt makes that us.
        Send(KeyEvent(0x12, true), KeyEvent(0x12, false));
        ShowWindow(hwnd, 5); // SW_SHOW
        BringWindowToTop(hwnd);
        if (!SetForegroundWindow(hwnd)) throw new Exception("SetForegroundWindow failed");
    }

    static void Shot(string path)
    {
        int vx = GetSystemMetrics(76), vy = GetSystemMetrics(77), vw = GetSystemMetrics(78), vh = GetSystemMetrics(79);
        using (var bmp = new Bitmap(vw, vh))
        {
            using (var g = Graphics.FromImage(bmp)) g.CopyFromScreen(vx, vy, 0, 0, new Size(vw, vh));
            bmp.Save(path, ImageFormat.Png);
        }
    }

    static int Main(string[] args)
    {
        SetProcessDPIAware();
        try
        {
            if (args.Length == 0) throw new Exception("no command");
            switch (args[0])
            {
                case "move": Move(int.Parse(args[1]), int.Parse(args[2])); break;
                case "down": Send(Mouse(ButtonFlag(args[1], true), 0, 0, 0)); break;
                case "up": Send(Mouse(ButtonFlag(args[1], false), 0, 0, 0)); break;
                case "click": Send(Mouse(ButtonFlag(args[1], true), 0, 0, 0), Mouse(ButtonFlag(args[1], false), 0, 0, 0)); break;
                case "wheel":
                    {
                        int n = int.Parse(args[1]);
                        for (int i = 0; i < Math.Abs(n); i++)
                        {
                            Send(Mouse(MOUSEEVENTF_WHEEL, 0, 0, unchecked((uint)(n > 0 ? -120 : 120))));
                            Thread.Sleep(30);
                        }
                        break;
                    }
                case "key": KeyCombo(args[1]); break;
                case "type": TypeText(string.Join(" ", args, 1, args.Length - 1)); break;
                case "activate": Activate(int.Parse(args[1])); break;
                case "resize":
                    SetWindowPos(MainWindow(int.Parse(args[1])), IntPtr.Zero, 0, 0, int.Parse(args[2]), int.Parse(args[3]), 0x2 | 0x4);
                    break;
                case "maximize": ShowWindow(MainWindow(int.Parse(args[1])), 3); break;
                case "shot": Shot(args[1]); break;
                default: throw new Exception("unknown command " + args[0]);
            }
            return 0;
        }
        catch (Exception e)
        {
            Console.Error.WriteLine("driver: " + e.Message);
            return 1;
        }
    }
}
