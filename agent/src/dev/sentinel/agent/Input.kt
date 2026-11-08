package dev.sentinel.agent

import android.os.SystemClock
import android.view.InputDevice
import android.view.InputEvent
import android.view.KeyCharacterMap
import android.view.KeyEvent
import android.view.MotionEvent
import java.io.DataInputStream
import java.io.InputStream
import java.lang.reflect.Method

/**
 * Control messages (desktop -> agent), big-endian:
 *   1 TOUCH   action u8 (0 down, 1 up, 2 move) | x i32 | y i32 | frame width u16 | frame height u16
 *   2 SCROLL  x i32 | y i32 | frame width u16 | frame height u16 | horizontal f32 | vertical f32
 *   3 KEY     action u8 (0 down, 1 up) | keycode i32 | meta state i32
 *   4 TEXT    length i32 | UTF-8 bytes
 *   5 KEY_FRAME_REQUEST  (no payload)
 * Positions are in the pixel space of the video frame the desktop is showing; events for a
 * frame size that no longer matches (mid-rotation) are dropped.
 */
class InputHandler(private val streamer: ScreenStreamer) {
    private val injector = Injector()
    private val keyMap: KeyCharacterMap by lazy { KeyCharacterMap.load(KeyCharacterMap.VIRTUAL_KEYBOARD) }
    private var touchDownTime = 0L

    fun serve(stream: InputStream) {
        val input = DataInputStream(stream.buffered())
        while (true) {
            when (val type = input.readUnsignedByte()) {
                1 -> touch(input.readUnsignedByte(), input.readInt(), input.readInt(), input.readUnsignedShort(), input.readUnsignedShort())
                2 -> scroll(input.readInt(), input.readInt(), input.readUnsignedShort(), input.readUnsignedShort(), input.readFloat(), input.readFloat())
                3 -> key(input.readUnsignedByte(), input.readInt(), input.readInt())
                4 -> {
                    val bytes = ByteArray(input.readInt())
                    input.readFully(bytes)
                    text(String(bytes, Charsets.UTF_8))
                }
                5 -> streamer.requestKeyFrame()
                else -> throw IllegalStateException("unknown control message $type")
            }
        }
    }

    /** Map a frame position to display coordinates, or null if the frame size is stale. */
    private fun toDisplay(x: Int, y: Int, frameW: Int, frameH: Int): Pair<Float, Float>? {
        val frame = streamer.frameSize
        if (frame.width != frameW || frame.height != frameH || frameW == 0 || frameH == 0) return null
        val display = streamer.displaySize
        return (x.toFloat() * display.width / frameW) to (y.toFloat() * display.height / frameH)
    }

    private fun touch(action: Int, x: Int, y: Int, frameW: Int, frameH: Int) {
        val (dx, dy) = toDisplay(x, y, frameW, frameH) ?: return
        val now = SystemClock.uptimeMillis()
        val motionAction = when (action) {
            0 -> MotionEvent.ACTION_DOWN.also { touchDownTime = now }
            1 -> MotionEvent.ACTION_UP
            else -> MotionEvent.ACTION_MOVE
        }
        val props = MotionEvent.PointerProperties().apply { id = 0; toolType = MotionEvent.TOOL_TYPE_FINGER }
        val coords = MotionEvent.PointerCoords().apply {
            this.x = dx
            this.y = dy
            pressure = if (motionAction == MotionEvent.ACTION_UP) 0f else 1f
            size = 1f
        }
        val event = MotionEvent.obtain(
            touchDownTime, now, motionAction, 1, arrayOf(props), arrayOf(coords),
            0, 0, 1f, 1f, 0, 0, InputDevice.SOURCE_TOUCHSCREEN, 0,
        )
        injector.inject(event)
        event.recycle()
    }

    private fun scroll(x: Int, y: Int, frameW: Int, frameH: Int, h: Float, v: Float) {
        val (dx, dy) = toDisplay(x, y, frameW, frameH) ?: return
        val now = SystemClock.uptimeMillis()
        val props = MotionEvent.PointerProperties().apply { id = 0; toolType = MotionEvent.TOOL_TYPE_MOUSE }
        val coords = MotionEvent.PointerCoords().apply {
            this.x = dx
            this.y = dy
            setAxisValue(MotionEvent.AXIS_HSCROLL, h)
            setAxisValue(MotionEvent.AXIS_VSCROLL, v)
        }
        val event = MotionEvent.obtain(
            now, now, MotionEvent.ACTION_SCROLL, 1, arrayOf(props), arrayOf(coords),
            0, 0, 1f, 1f, 0, 0, InputDevice.SOURCE_MOUSE, 0,
        )
        injector.inject(event)
        event.recycle()
    }

    private fun key(action: Int, keycode: Int, meta: Int) {
        val now = SystemClock.uptimeMillis()
        val keyAction = if (action == 0) KeyEvent.ACTION_DOWN else KeyEvent.ACTION_UP
        injector.inject(
            KeyEvent(now, now, keyAction, keycode, 0, meta, KeyCharacterMap.VIRTUAL_KEYBOARD, 0, 0, InputDevice.SOURCE_KEYBOARD),
        )
    }

    private fun text(text: String) {
        for (ch in text) {
            // Characters the virtual keyboard map can't produce are skipped.
            val events = keyMap.getEvents(charArrayOf(ch)) ?: continue
            for (e in events) {
                val now = SystemClock.uptimeMillis()
                injector.inject(KeyEvent.changeTimeRepeat(e, now, 0))
            }
        }
    }
}

/** InputManager's hidden injectInputEvent, from InputManagerGlobal on Android 14+. */
private class Injector {
    private val manager: Any
    private val inject: Method

    init {
        var found: Pair<Any, Method>? = null
        for (className in listOf("android.hardware.input.InputManagerGlobal", "android.hardware.input.InputManager")) {
            try {
                val cls = Class.forName(className)
                val instance = cls.getMethod("getInstance").invoke(null) ?: continue
                found = instance to cls.getMethod("injectInputEvent", InputEvent::class.java, Int::class.javaPrimitiveType)
                break
            } catch (_: Throwable) {
            }
        }
        val (m, i) = found ?: throw IllegalStateException("no input manager available")
        manager = m
        inject = i
    }

    fun inject(event: InputEvent) {
        // Mode 0 = INJECT_INPUT_EVENT_MODE_ASYNC: don't wait for the app to handle it.
        inject.invoke(manager, event, 0)
    }
}
