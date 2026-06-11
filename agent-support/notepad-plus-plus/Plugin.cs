using System;
using System.Collections.Concurrent;
using System.Diagnostics;
using System.IO;
using System.Runtime.InteropServices;
using System.Text;
using System.Threading;
using Kbg.NppPluginNET.PluginInfrastructure;

namespace GitAi.NotepadPlusPlus
{
    /// <summary>
    /// git-ai Notepad++ plugin — fires known_human checkpoint on file save.
    ///
    /// Installation:
    ///   Copy git-ai.dll to %APPDATA%\Notepad++\plugins\git-ai\git-ai.dll
    ///   Restart Notepad++.
    /// </summary>
    public class Plugin
    {
        public const string PluginName = "git-ai";

        private static IntPtr _nppHandle = IntPtr.Zero;
        private static readonly ConcurrentDictionary<string, Timer> _timers = new();
        private static readonly ConcurrentDictionary<string, ConcurrentDictionary<string, string>> _pending = new();
        private const int DebounceMs = 500;

        private static string GitAiBin
        {
            get
            {
                var home = Environment.GetFolderPath(Environment.SpecialFolder.UserProfile);
                var exe = Path.Combine(home, ".git-ai", "bin", "git-ai.exe");
                return File.Exists(exe) ? exe : "git-ai";
            }
        }

        /// Called by the plugin host to set up commands. We have no menu items.
        public static void CommandMenuInit() { }

        /// Called by Notepad++ with notification messages.
        public static void beNotified(ScNotification notification)
        {
            if (notification.Header.Code == (uint)NppMsg.NPPN_NATIVELANGCHANGED)
            {
                // No-op
            }
            else if (notification.Header.Code == (uint)NppMsg.NPPN_FILESAVED)
            {
                OnFileSaved();
            }
            else if (notification.Header.Code == (uint)NppMsg.NPPN_SHUTDOWN)
            {
                // Cancel all pending timers
                foreach (var timer in _timers.Values)
                    timer?.Dispose();
                _timers.Clear();
                _pending.Clear();
            }
        }

        private static void OnFileSaved()
        {
            // Get the current file path via NPPM_GETFULLCURRENTPATH
            var filePath = GetCurrentFilePath();
            if (string.IsNullOrEmpty(filePath)) return;

            var norm = filePath.Replace('\\', '/');
            if (norm.Contains("/.git/")) return;

            var repoRoot = FindRepoRoot(Path.GetDirectoryName(filePath));
            if (repoRoot == null) return;

            string content;
            try { content = File.ReadAllText(filePath, Encoding.UTF8); }
            catch { return; }

            var files = _pending.GetOrAdd(repoRoot, _ => new ConcurrentDictionary<string, string>());
            files[filePath] = content;

            _timers.AddOrUpdate(repoRoot,
                _ =>
                {
                    var t = new Timer(_ => FireCheckpoint(repoRoot), null, DebounceMs, Timeout.Infinite);
                    return t;
                },
                (_, existing) =>
                {
                    existing.Change(DebounceMs, Timeout.Infinite);
                    return existing;
                });
        }

        private static string? GetCurrentFilePath()
        {
            if (_nppHandle == IntPtr.Zero) return null;
            var sb = new StringBuilder(Win32.MAX_PATH);
            Win32.SendMessage(_nppHandle, NppMsg.NPPM_GETFULLCURRENTPATH,
                (IntPtr)Win32.MAX_PATH, sb);
            var result = sb.ToString();
            return string.IsNullOrEmpty(result) ? null : result;
        }

        private static void FireCheckpoint(string repoRoot)
        {
            if (!_pending.TryRemove(repoRoot, out var files) || files.IsEmpty) return;

            var sb = new StringBuilder();
            sb.Append("{\"editor\":\"notepad-plus-plus\",\"editor_version\":\"unknown\",");
            sb.Append("\"extension_version\":\"1.0.0\",");
            sb.Append("\"cwd\":\"").Append(EscapeJson(repoRoot)).Append("\",");
            sb.Append("\"edited_filepaths\":[");
            bool first = true;
            foreach (var p in files.Keys)
            {
                if (!first) sb.Append(',');
                first = false;
                sb.Append('"').Append(EscapeJson(p)).Append('"');
            }
            sb.Append("],\"dirty_files\":{");
            first = true;
            foreach (var kv in files)
            {
                if (!first) sb.Append(',');
                first = false;
                sb.Append('"').Append(EscapeJson(kv.Key)).Append("\":\"")
                  .Append(EscapeJson(kv.Value)).Append('"');
            }
            sb.Append("}}");

            try
            {
                var psi = new ProcessStartInfo(GitAiBin,
                    "checkpoint known_human --hook-input stdin")
                {
                    UseShellExecute = false,
                    RedirectStandardInput = true,
                    RedirectStandardOutput = false,
                    RedirectStandardError = false,
                    CreateNoWindow = true,
                    WorkingDirectory = repoRoot,
                };
                using var proc = Process.Start(psi);
                if (proc == null) return;
                proc.StandardInput.Write(sb.ToString());
                proc.StandardInput.Close();
                proc.WaitForExit(10000);
            }
            catch { /* best-effort */ }
        }

        private static string? FindRepoRoot(string? dir)
        {
            var d = dir;
            while (!string.IsNullOrEmpty(d))
            {
                if (Directory.Exists(Path.Combine(d, ".git"))) return d;
                var parent = Path.GetDirectoryName(d);
                if (parent == d) break;
                d = parent;
            }
            return null;
        }

        private static string EscapeJson(string s) =>
            s.Replace("\\", "\\\\").Replace("\"", "\\\"")
             .Replace("\n", "\\n").Replace("\r", "\\r").Replace("\t", "\\t");

        /// Called by the plugin loader with the Notepad++ window handle.
        public static void SetToolBarIcon() { }
        public static IntPtr GetNppHandle() => _nppHandle;

        [DllExport(CallingConvention = CallingConvention.Cdecl)]
        public static void setInfo(NppData notepadPlusData)
        {
            _nppHandle = notepadPlusData._nppHandle;
        }

        private static readonly IntPtr _namePtr = Marshal.StringToHGlobalUni(PluginName);

        [DllExport(CallingConvention = CallingConvention.Cdecl)]
        public static IntPtr getName() => _namePtr;

        [DllExport(CallingConvention = CallingConvention.Cdecl)]
        public static FuncItem[] getFuncsArray(ref int nbF)
        {
            nbF = 0;
            return Array.Empty<FuncItem>();
        }

        [DllExport(CallingConvention = CallingConvention.Cdecl)]
        public static void beNotified(IntPtr notifyCode)
        {
            var sc = (ScNotification)Marshal.PtrToStructure(notifyCode, typeof(ScNotification))!;
            beNotified(sc);
        }

        [DllExport(CallingConvention = CallingConvention.Cdecl)]
        public static IntPtr messageProc(uint msg, IntPtr wParam, IntPtr lParam) => IntPtr.Zero;

        [DllExport(CallingConvention = CallingConvention.Cdecl)]
        public static bool isUnicode() => true;
    }
}
