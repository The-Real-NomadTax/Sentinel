package dev.sentinel.agent

import android.content.pm.ApplicationInfo
import android.content.res.Configuration
import android.content.res.Resources
import android.os.Looper

/**
 * We run as a bare `app_process`, not an app, so some framework code finds no current
 * application. Give it a main looper and an ActivityThread that reports the shell package,
 * which is who we really are.
 */
object Workarounds {
    fun apply() {
        try {
            Looper.prepareMainLooper()
        } catch (e: Throwable) {
            log("prepareMainLooper: $e")
        }
        try {
            val atClass = Class.forName("android.app.ActivityThread")
            val activityThread = atClass.getDeclaredConstructor().apply { isAccessible = true }.newInstance()
            atClass.getDeclaredField("sCurrentActivityThread").apply { isAccessible = true }
                .set(null, activityThread)

            val bindDataClass = Class.forName("android.app.ActivityThread\$AppBindData")
            val bindData = bindDataClass.getDeclaredConstructor().apply { isAccessible = true }.newInstance()
            val appInfo = ApplicationInfo().apply { packageName = "com.android.shell" }
            bindDataClass.getDeclaredField("appInfo").apply { isAccessible = true }.set(bindData, appInfo)
            atClass.getDeclaredField("mBoundApplication").apply { isAccessible = true }.set(activityThread, bindData)

            // Vendor display hooks (Samsung's CompatSandbox) ask the ActivityThread for its
            // configuration; give it a controller holding the system configuration.
            try {
                val ccClass = Class.forName("android.app.ConfigurationController")
                val internal = Class.forName("android.app.ActivityThreadInternal")
                val controller = ccClass.getDeclaredConstructor(internal).apply { isAccessible = true }
                    .newInstance(activityThread)
                ccClass.getDeclaredField("mConfiguration").apply { isAccessible = true }
                    .set(controller, Configuration(Resources.getSystem().configuration))
                atClass.getDeclaredField("mConfigurationController").apply { isAccessible = true }
                    .set(activityThread, controller)
            } catch (e: Throwable) {
                log("configuration workaround failed: $e")
            }
        } catch (e: Throwable) {
            // Not fatal: many Android versions don't need it.
            log("activity thread workaround failed: $e")
        }
    }
}
