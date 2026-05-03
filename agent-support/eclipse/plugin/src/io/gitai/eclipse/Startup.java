package io.gitai.eclipse;

import org.eclipse.core.resources.IResourceChangeEvent;
import org.eclipse.core.resources.ResourcesPlugin;
import org.eclipse.ui.IStartup;

public class Startup implements IStartup {
    @Override
    public void earlyStartup() {
        GitAiSaveListener listener = new GitAiSaveListener();
        ResourcesPlugin.getWorkspace().addResourceChangeListener(
            listener, IResourceChangeEvent.POST_CHANGE);
        Activator.getDefault().setListener(listener);
    }
}
