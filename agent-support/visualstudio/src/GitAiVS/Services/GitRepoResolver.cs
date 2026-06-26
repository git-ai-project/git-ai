using System;
using System.IO;
using System.Runtime.InteropServices;

namespace GitAiVS.Services
{
    /// <summary>
    /// Finds the git repository root for a given file path by walking up
    /// the directory tree looking for a .git directory.
    /// </summary>
    public static class GitRepoResolver
    {
        private static readonly StringComparison PathComparison =
            RuntimeInformation.IsOSPlatform(OSPlatform.Windows)
                ? StringComparison.OrdinalIgnoreCase
                : StringComparison.Ordinal;

        public static string? FindRepoRoot(string filePath)
        {
            var dir = Path.GetDirectoryName(filePath);

            while (dir != null)
            {
                if (Directory.Exists(Path.Combine(dir, ".git")) || File.Exists(Path.Combine(dir, ".git")))
                    return dir;

                dir = Path.GetDirectoryName(dir);
            }

            return null;
        }

        /// <summary>
        /// Convert an absolute file path to a path relative to the workspace root.
        /// Uses case-insensitive comparison on Windows to handle path casing mismatches.
        /// </summary>
        internal static string ToRelativePath(string absolutePath, string workspaceRoot)
        {
            if (string.IsNullOrEmpty(workspaceRoot))
                return absolutePath;

            var normalizedRoot = TrimTrailingSeparators(workspaceRoot);

            if (absolutePath.Equals(normalizedRoot, PathComparison))
                return string.Empty;

            if (EndsWithSeparator(normalizedRoot))
            {
                return absolutePath.StartsWith(normalizedRoot, PathComparison)
                    ? absolutePath.Substring(normalizedRoot.Length)
                    : absolutePath;
            }

            if (absolutePath.StartsWith(normalizedRoot, PathComparison)
                && absolutePath.Length > normalizedRoot.Length
                && IsPathSeparator(absolutePath[normalizedRoot.Length]))
            {
                return absolutePath.Substring(normalizedRoot.Length + 1);
            }

            return absolutePath;
        }

        private static string TrimTrailingSeparators(string path)
        {
            while (path.Length > 1 && EndsWithSeparator(path) && !IsWindowsDriveRoot(path))
                path = path.Substring(0, path.Length - 1);

            return path;
        }

        private static bool EndsWithSeparator(string path)
        {
            return path.Length > 0 && IsPathSeparator(path[path.Length - 1]);
        }

        private static bool IsPathSeparator(char value)
        {
            return value == Path.DirectorySeparatorChar || value == Path.AltDirectorySeparatorChar;
        }

        private static bool IsWindowsDriveRoot(string path)
        {
            return path.Length == 3
                && char.IsLetter(path[0])
                && path[1] == ':'
                && IsPathSeparator(path[2]);
        }
    }
}
