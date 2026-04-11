package io.gitai.eclipse;

import org.eclipse.core.resources.*;
import org.eclipse.core.runtime.CoreException;

import java.io.*;
import java.nio.charset.StandardCharsets;
import java.util.*;
import java.util.concurrent.*;

public class GitAiSaveListener implements IResourceChangeListener {

    private static final long DEBOUNCE_MS = 500;
    private static final String GIT_AI_BIN = resolveGitAiBin();

    private final ScheduledExecutorService scheduler =
            Executors.newSingleThreadScheduledExecutor(r -> {
                Thread t = new Thread(r, "git-ai-debounce");
                t.setDaemon(true);
                return t;
            });

    private final ExecutorService ioExecutor = Executors.newCachedThreadPool(r -> {
        Thread t = new Thread(r, "git-ai-checkpoint-io");
        t.setDaemon(true);
        return t;
    });

    // per repo root -> debounce future
    private final ConcurrentHashMap<String, ScheduledFuture<?>> futures = new ConcurrentHashMap<>();
    // per repo root -> accumulated files {path -> content}
    private final ConcurrentHashMap<String, ConcurrentHashMap<String, String>> pending =
            new ConcurrentHashMap<>();

    private static String resolveGitAiBin() {
        String home = System.getProperty("user.home");
        String bin = home + "/.git-ai/bin/git-ai";
        if (new File(bin).exists()) return bin;
        // Windows
        bin = home + "\\.git-ai\\bin\\git-ai.exe";
        if (new File(bin).exists()) return bin;
        return "git-ai"; // fallback to PATH
    }

    @Override
    public void resourceChanged(IResourceChangeEvent event) {
        if (event.getDelta() == null) return;
        Set<String> repoRoots = new HashSet<>();
        collectChanges(event.getDelta(), repoRoots);
        for (String root : repoRoots) {
            scheduleCheckpoint(root);
        }
    }

    private void collectChanges(IResourceDelta delta, Set<String> repoRoots) {
        IResource resource = delta.getResource();
        if (resource instanceof IFile) {
            IFile file = (IFile) resource;
            if ((delta.getFlags() & IResourceDelta.CONTENT) != 0 &&
                    (delta.getKind() & IResourceDelta.CHANGED) != 0) {
                String path = file.getLocation().toOSString();
                if (path.contains("/.git/") || path.contains("\\.git\\")) return;
                String root = findRepoRoot(file.getParent());
                if (root == null) return;
                pending.computeIfAbsent(root, k -> new ConcurrentHashMap<>())
                        .put(path, readFileContent(file));
                repoRoots.add(root);
            }
        }
        for (IResourceDelta child : delta.getAffectedChildren()) {
            collectChanges(child, repoRoots);
        }
    }

    private String readFileContent(IFile file) {
        try (InputStream is = file.getContents()) {
            return new String(is.readAllBytes(), StandardCharsets.UTF_8);
        } catch (Exception e) {
            return "";
        }
    }

    private String findRepoRoot(IContainer container) {
        if (container == null) return null;
        if (container instanceof IWorkspaceRoot) return null;
        if (container.findMember(".git") != null) {
            return container.getLocation().toOSString();
        }
        return findRepoRoot(container.getParent());
    }

    private void scheduleCheckpoint(String repoRoot) {
        futures.compute(repoRoot, (key, existing) -> {
            if (existing != null) existing.cancel(false);
            return scheduler.schedule(() -> fireCheckpoint(repoRoot), DEBOUNCE_MS, TimeUnit.MILLISECONDS);
        });
    }

    private void fireCheckpoint(String repoRoot) {
        futures.remove(repoRoot);
        Map<String, String> files = pending.remove(repoRoot);
        if (files == null || files.isEmpty()) return;

        Map<String, Object> payload = new LinkedHashMap<>();
        payload.put("editor", "eclipse");
        payload.put("editor_version", System.getProperty("eclipse.buildId", "unknown"));
        payload.put("extension_version", "1.0.0");
        payload.put("cwd", repoRoot);
        payload.put("edited_filepaths", new ArrayList<>(files.keySet()));
        payload.put("dirty_files", files);

        String json = toJson(payload);
        ioExecutor.submit(() -> {
            try {
                ProcessBuilder pb = new ProcessBuilder(
                        GIT_AI_BIN, "checkpoint", "known_human", "--hook-input", "stdin");
                pb.directory(new File(repoRoot));
                pb.redirectErrorStream(true);
                Process proc = pb.start();
                try (OutputStream stdin = proc.getOutputStream()) {
                    stdin.write(json.getBytes(StandardCharsets.UTF_8));
                }
                proc.waitFor(10, TimeUnit.SECONDS);
            } catch (Exception e) {
                // Best-effort; don't surface errors to the user
            }
        });
    }

    public void shutdown() {
        scheduler.shutdownNow();
        ioExecutor.shutdownNow();
        ResourcesPlugin.getWorkspace().removeResourceChangeListener(this);
    }

    // Simple JSON builder — no external deps needed
    private static String toJson(Map<String, Object> map) {
        StringBuilder sb = new StringBuilder("{");
        boolean first = true;
        for (Map.Entry<String, Object> e : map.entrySet()) {
            if (!first) sb.append(",");
            first = false;
            sb.append("\"").append(e.getKey()).append("\":");
            appendValue(sb, e.getValue());
        }
        sb.append("}");
        return sb.toString();
    }

    @SuppressWarnings("unchecked")
    private static void appendValue(StringBuilder sb, Object v) {
        if (v instanceof String) {
            sb.append("\"").append(((String) v)
                .replace("\\", "\\\\").replace("\"", "\\\"")
                .replace("\n", "\\n").replace("\r", "\\r")
                .replace("\t", "\\t")).append("\"");
        } else if (v instanceof List) {
            sb.append("[");
            boolean f = true;
            for (Object item : (List<?>) v) { if (!f) sb.append(","); f = false; appendValue(sb, item); }
            sb.append("]");
        } else if (v instanceof Map) {
            sb.append(toJson((Map<String, Object>) v));
        } else {
            sb.append(v);
        }
    }
}
