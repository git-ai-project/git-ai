using System;
using System.Collections.Concurrent;
using System.Collections.Generic;
using System.IO;
using System.Text;
using System.Threading;
using System.Threading.Tasks;
using Microsoft.VisualStudio;
using Microsoft.VisualStudio.Shell;
using Microsoft.VisualStudio.Shell.Interop;

namespace GitAi.VisualStudio
{
    internal sealed class DocumentSaveListener : IVsRunningDocTableEvents3, IDisposable
    {
        private readonly AsyncPackage _package;
        private IVsRunningDocumentTable _rdt;
        private uint _rdtCookie;

        // Per-repo-root debounce state
        private readonly ConcurrentDictionary<string, Timer> _timers = new();
        private readonly ConcurrentDictionary<string, ConcurrentDictionary<string, string>> _pending = new();

        private const int DebounceMs = 500;

        private static string GitAiBin
        {
            get
            {
                var home = Environment.GetFolderPath(Environment.SpecialFolder.UserProfile);
                var p = Path.Combine(home, ".git-ai", "bin", "git-ai.exe");
                return File.Exists(p) ? p : "git-ai";
            }
        }

        public DocumentSaveListener(AsyncPackage package) => _package = package;

        public async Task InitializeAsync()
        {
            await _package.JoinableTaskFactory.SwitchToMainThreadAsync();
            _rdt = await _package.GetServiceAsync(typeof(SVsRunningDocumentTable))
                as IVsRunningDocumentTable;
            if (_rdt == null) return;
            _rdt.AdviseRunningDocTableEvents(this, out _rdtCookie);
        }

        public void Dispose()
        {
            ThreadHelper.ThrowIfNotOnUIThread();
            if (_rdtCookie != 0)
            {
                _rdt?.UnadviseRunningDocTableEvents(_rdtCookie);
                _rdtCookie = 0;
            }
            foreach (var t in _timers.Values) t.Dispose();
            _timers.Clear();
        }

        // IVsRunningDocTableEvents3
        public int OnAfterSave(uint docCookie)
        {
            ThreadHelper.ThrowIfNotOnUIThread();
            if (_rdt == null) return VSConstants.S_OK;

            _rdt.GetDocumentInfo(docCookie,
                out _, out _, out _, out string filePath,
                out _, out _, out _);

            if (string.IsNullOrEmpty(filePath)) return VSConstants.S_OK;
            var norm = filePath.Replace('\\', '/');
            if (norm.Contains("/.git/")) return VSConstants.S_OK;

            var repoRoot = FindRepoRoot(Path.GetDirectoryName(filePath));
            if (repoRoot == null) return VSConstants.S_OK;

            string content;
            try { content = File.ReadAllText(filePath, Encoding.UTF8); }
            catch { return VSConstants.S_OK; }

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

            return VSConstants.S_OK;
        }

        private void FireCheckpoint(string repoRoot)
        {
            _timers.TryRemove(repoRoot, out var oldTimer);
            oldTimer?.Dispose();
            if (!_pending.TryRemove(repoRoot, out var files) || files.IsEmpty) return;

            var pathsList = new List<string>(files.Keys);
            var sb = new StringBuilder();
            sb.Append("{\"editor\":\"visual-studio\",\"editor_version\":\"unknown\",");
            sb.Append("\"extension_version\":\"1.0.0\",");
            sb.Append("\"cwd\":\"").Append(EscapeJson(repoRoot)).Append("\",");
            sb.Append("\"edited_filepaths\":[");
            bool first = true;
            foreach (var p in pathsList)
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
                var psi = new System.Diagnostics.ProcessStartInfo(GitAiBin,
                    "checkpoint known_human --hook-input stdin")
                {
                    UseShellExecute = false,
                    RedirectStandardInput = true,
                    RedirectStandardOutput = true,
                    RedirectStandardError = true,
                    CreateNoWindow = true,
                    WorkingDirectory = repoRoot,
                };
                using var proc = System.Diagnostics.Process.Start(psi);
                if (proc == null) return;
                proc.StandardInput.Write(sb.ToString());
                proc.StandardInput.Close();
                proc.WaitForExit(10000);
            }
            catch { /* best-effort */ }
        }

        private static string FindRepoRoot(string dir)
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

        private static string EscapeJson(string s)
        {
            var sb = new System.Text.StringBuilder();
            foreach (char c in s)
            {
                switch (c)
                {
                    case '\\': sb.Append("\\\\"); break;
                    case '"': sb.Append("\\\""); break;
                    case '\n': sb.Append("\\n"); break;
                    case '\r': sb.Append("\\r"); break;
                    case '\t': sb.Append("\\t"); break;
                    case '\b': sb.Append("\\b"); break;
                    case '\f': sb.Append("\\f"); break;
                    default:
                        if (c < 0x20)
                            sb.AppendFormat("\\u{0:X4}", (int)c);
                        else
                            sb.Append(c);
                        break;
                }
            }
            return sb.ToString();
        }

        // Unused interface members — return S_OK
        public int OnAfterFirstDocumentLock(uint docCookie, uint dwRDTLockType, uint dwReadLocksRemaining, uint dwEditLocksRemaining) => VSConstants.S_OK;
        public int OnBeforeLastDocumentUnlock(uint docCookie, uint dwRDTLockType, uint dwReadLocksRemaining, uint dwEditLocksRemaining) => VSConstants.S_OK;
        public int OnAfterAttributeChange(uint docCookie, uint grfAttribs) => VSConstants.S_OK;
        public int OnBeforeDocumentWindowShow(uint docCookie, int fFirstShow, Microsoft.VisualStudio.Shell.Interop.IVsWindowFrame pFrame) => VSConstants.S_OK;
        public int OnAfterDocumentWindowHide(uint docCookie, Microsoft.VisualStudio.Shell.Interop.IVsWindowFrame pFrame) => VSConstants.S_OK;
        public int OnAfterAttributeChangeEx(uint docCookie, uint grfAttribs, IVsHierarchy pHierOld, uint itemidOld, string pszMkDocumentOld, IVsHierarchy pHierNew, uint itemidNew, string pszMkDocumentNew) => VSConstants.S_OK;
        public int OnBeforeSave(uint docCookie) => VSConstants.S_OK;
    }
}
