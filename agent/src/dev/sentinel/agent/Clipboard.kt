package dev.sentinel.agent

import android.content.ClipData
import android.os.IBinder

/**
 * The phone's clipboard. We have no Context, so talk to the clipboard service's hidden binder
 * interface directly, as the shell package (which is who we really are).
 */
object Clipboard {
    private const val SHELL_PACKAGE = "com.android.shell"
    private val UNSUPPORTED = Any()

    private val service: Any? by lazy {
        runCatching {
            val binder = Class.forName("android.os.ServiceManager").getMethod("getService", String::class.java)
                .invoke(null, "clipboard") as IBinder
            Class.forName("android.content.IClipboard\$Stub")
                .getMethod("asInterface", IBinder::class.java).invoke(null, binder)
        }.onFailure { log("clipboard unavailable: $it") }.getOrNull()
    }

    /** Put [text] on the clipboard. Returns false if that isn't possible on this device. */
    fun set(text: String): Boolean {
        val service = service ?: return false
        val clip = ClipData.newPlainText("Sentinel", text)
        // `setPrimaryClip`'s parameters differ between Android versions and vendors, e.g.
        //   (ClipData, String callingPackage)
        //   (ClipData, String callingPackage, int userId)
        //   (ClipData, String callingPackage, String attributionTag, int userId[, int deviceId])
        // so fill them by type: our package for the first String, null for later ones, and 0
        // (the main user, the default device) for ints.
        val candidates = service.javaClass.methods.filter { it.name == "setPrimaryClip" }.sortedBy { it.parameterCount }
        for (method in candidates) {
            var sawString = false
            val args = method.parameterTypes.map { type ->
                when (type) {
                    ClipData::class.java -> clip
                    String::class.java -> if (sawString) null else SHELL_PACKAGE.also { sawString = true }
                    Int::class.javaPrimitiveType -> 0
                    Boolean::class.javaPrimitiveType -> false
                    else -> UNSUPPORTED
                }
            }
            if (args.any { it === UNSUPPORTED }) continue
            try {
                method.invoke(service, *args.toTypedArray())
                return true
            } catch (e: Throwable) {
                log("setPrimaryClip(${method.parameterCount} args) failed: ${e.cause ?: e}")
            }
        }
        return false
    }
}
