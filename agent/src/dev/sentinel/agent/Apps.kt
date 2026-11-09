package dev.sentinel.agent

import android.content.Context
import android.content.pm.ApplicationInfo
import android.content.pm.PackageInfo
import android.content.pm.PackageManager
import android.os.Build
import java.io.BufferedWriter
import java.io.File
import java.io.OutputStreamWriter

/**
 * `mode=apps`: print every installed package, one per line, then exit. Used by the desktop to
 * list apps with their real names (which only the framework can read from each APK).
 *
 * Line format (tab-separated, fields never contain tabs or newlines):
 *   APP | package | label | version name | version code | system 0/1 | enabled 0/1 | apk paths ('|'-separated) | total bytes
 */
object Apps {
    fun list() {
        val pm = packageManager()
        val packages: List<PackageInfo> = if (Build.VERSION.SDK_INT >= 33) {
            pm.getInstalledPackages(PackageManager.PackageInfoFlags.of(0))
        } else {
            @Suppress("DEPRECATION")
            pm.getInstalledPackages(0)
        }
        val out = BufferedWriter(OutputStreamWriter(System.out, Charsets.UTF_8), 1 shl 16)
        for (pkg in packages) {
            val app = pkg.applicationInfo ?: continue
            val paths = listOfNotNull(app.sourceDir) + (app.splitSourceDirs?.toList() ?: emptyList())
            if (paths.isEmpty()) continue
            val label = runCatching { app.loadLabel(pm).toString() }.getOrNull()
                ?.takeIf { it.isNotBlank() } ?: pkg.packageName
            val versionCode = if (Build.VERSION.SDK_INT >= 28) pkg.longVersionCode else {
                @Suppress("DEPRECATION")
                pkg.versionCode.toLong()
            }
            val system = app.flags and (ApplicationInfo.FLAG_SYSTEM or ApplicationInfo.FLAG_UPDATED_SYSTEM_APP) != 0
            val bytes = paths.sumOf { File(it).length() }
            val fields = listOf(
                "APP",
                pkg.packageName,
                label,
                pkg.versionName ?: "",
                versionCode.toString(),
                if (system) "1" else "0",
                if (app.enabled) "1" else "0",
                paths.joinToString("|"),
                bytes.toString(),
            )
            out.write(fields.joinToString("\t") { clean(it) })
            out.write("\n")
        }
        out.flush()
    }

    private fun clean(s: String) = s.replace('\t', ' ').replace('\n', ' ').replace('\r', ' ')

    /** A PackageManager from the system context of the ActivityThread set up by [Workarounds]. */
    private fun packageManager(): PackageManager {
        val atClass = Class.forName("android.app.ActivityThread")
        val thread = atClass.getMethod("currentActivityThread").invoke(null)
        val context = atClass.getMethod("getSystemContext").invoke(thread) as Context
        return context.packageManager
    }
}
