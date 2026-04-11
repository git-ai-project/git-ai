package io.gitai.eclipse;

import org.eclipse.core.runtime.Plugin;
import org.osgi.framework.BundleContext;

public class Activator extends Plugin {
    public static final String PLUGIN_ID = "io.gitai.eclipse";
    private static Activator instance;

    private volatile GitAiSaveListener listener;

    @Override
    public void start(BundleContext context) throws Exception {
        super.start(context);
        instance = this;
    }

    @Override
    public void stop(BundleContext context) throws Exception {
        if (listener != null) {
            listener.shutdown();
            listener = null;
        }
        instance = null;
        super.stop(context);
    }

    public void setListener(GitAiSaveListener listener) {
        this.listener = listener;
    }

    public static Activator getDefault() { return instance; }
}
