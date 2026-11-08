package dev.sentinel.agent

import android.graphics.Rect
import android.hardware.display.DisplayManager
import android.hardware.display.VirtualDisplay
import android.os.IBinder
import android.view.Surface

/** Size of the main display in its current orientation. */
data class DisplaySize(val width: Int, val height: Int)

object Displays {
    // Talk to the display service directly. The client-side DisplayManagerGlobal wrapper runs
    // vendor hooks (e.g. Samsung's CompatSandbox) that expect a real app and crash here.
    private val displayService: Any by lazy {
        val binder = Class.forName("android.os.ServiceManager").getMethod("getService", String::class.java)
            .invoke(null, "display") as IBinder
        Class.forName("android.hardware.display.IDisplayManager\$Stub")
            .getMethod("asInterface", IBinder::class.java).invoke(null, binder)!!
    }

    fun mainDisplaySize(): DisplaySize {
        val info = displayService.javaClass.getMethod("getDisplayInfo", Int::class.javaPrimitiveType)
            .invoke(displayService, 0)!!
        val cls = info.javaClass
        return DisplaySize(cls.getField("logicalWidth").getInt(info), cls.getField("logicalHeight").getInt(info))
    }

    /**
     * Mirror the main display onto [surface] at [width]x[height]. Returns something to release.
     */
    fun mirror(surface: Surface, display: DisplaySize, width: Int, height: Int): AutoCloseable {
        // Android 14+: a hidden static helper creates an auto-mirroring virtual display.
        try {
            val method = DisplayManager::class.java.getMethod(
                "createVirtualDisplay",
                String::class.java,
                Int::class.javaPrimitiveType,
                Int::class.javaPrimitiveType,
                Int::class.javaPrimitiveType,
                Surface::class.java,
            )
            val virtualDisplay = method.invoke(null, "sentinel", width, height, 0, surface) as VirtualDisplay
            log("display: virtual display ${width}x$height")
            return AutoCloseable { virtualDisplay.release() }
        } catch (e: Throwable) {
            log("virtual display unavailable (${e.cause ?: e}), trying SurfaceControl")
        }
        return surfaceControlMirror(surface, display, width, height)
    }

    /** Older Android: build a display directly with SurfaceControl's hidden API. */
    private fun surfaceControlMirror(surface: Surface, display: DisplaySize, width: Int, height: Int): AutoCloseable {
        val sc = Class.forName("android.view.SurfaceControl")
        val token = sc.getMethod("createDisplay", String::class.java, Boolean::class.javaPrimitiveType)
            .invoke(null, "sentinel", false) as IBinder
        sc.getMethod("openTransaction").invoke(null)
        try {
            sc.getMethod("setDisplaySurface", IBinder::class.java, Surface::class.java).invoke(null, token, surface)
            sc.getMethod(
                "setDisplayProjection",
                IBinder::class.java,
                Int::class.javaPrimitiveType,
                Rect::class.java,
                Rect::class.java,
            ).invoke(null, token, 0, Rect(0, 0, display.width, display.height), Rect(0, 0, width, height))
            sc.getMethod("setDisplayLayerStack", IBinder::class.java, Int::class.javaPrimitiveType)
                .invoke(null, token, 0)
        } finally {
            sc.getMethod("closeTransaction").invoke(null)
        }
        log("display: SurfaceControl ${width}x$height")
        return AutoCloseable { sc.getMethod("destroyDisplay", IBinder::class.java).invoke(null, token) }
    }
}
