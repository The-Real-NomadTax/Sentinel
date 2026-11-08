package dev.sentinel.agent

import android.media.MediaCodec
import android.media.MediaCodecInfo
import android.media.MediaFormat
import android.os.Bundle
import java.io.DataOutputStream

/**
 * Encodes the main display to H.264 and writes it to the video socket. Restarts the encoder
 * whenever the display size changes (rotation), announcing the new size with a session packet.
 */
class ScreenStreamer(
    private val out: DataOutputStream,
    private val maxSize: Int,
    private val bitRate: Int,
    private val maxFps: Int,
) {
    /** Size of the frames currently being sent, and of the display they show. */
    @Volatile var frameSize = DisplaySize(0, 0)
        private set
    @Volatile var displaySize = DisplaySize(0, 0)
        private set

    @Volatile private var resetRequested = false
    @Volatile private var codec: MediaCodec? = null
    private var buffer = ByteArray(1 shl 16)

    fun run() {
        startRotationWatcher()
        while (true) {
            resetRequested = false
            val display = Displays.mainDisplaySize()
            val frame = scaled(display)
            displaySize = display
            frameSize = frame
            writeSession(frame)

            val codec = MediaCodec.createEncoderByType(MediaFormat.MIMETYPE_VIDEO_AVC)
            codec.configure(format(frame), null, null, MediaCodec.CONFIGURE_FLAG_ENCODE)
            val surface = codec.createInputSurface()
            val mirror = Displays.mirror(surface, display, frame.width, frame.height)
            codec.start()
            this.codec = codec
            try {
                encode(codec)
            } finally {
                this.codec = null
                runCatching { codec.stop() }
                codec.release()
                runCatching { mirror.close() }
                surface.release()
            }
        }
    }

    private fun encode(codec: MediaCodec) {
        val info = MediaCodec.BufferInfo()
        while (!resetRequested) {
            val index = codec.dequeueOutputBuffer(info, 100_000)
            if (index < 0) continue // timeout or format change
            try {
                if (info.size > 0) {
                    val data = codec.getOutputBuffer(index)!!
                    data.position(info.offset)
                    data.limit(info.offset + info.size)
                    var flags = info.presentationTimeUs and ((1L shl 61) - 1)
                    if (info.flags and MediaCodec.BUFFER_FLAG_CODEC_CONFIG != 0) flags = flags or (1L shl 62)
                    if (info.flags and MediaCodec.BUFFER_FLAG_KEY_FRAME != 0) flags = flags or (1L shl 61)
                    if (buffer.size < info.size) buffer = ByteArray(info.size)
                    data.get(buffer, 0, info.size)
                    out.writeLong(flags)
                    out.writeInt(info.size)
                    out.write(buffer, 0, info.size)
                    out.flush()
                }
            } finally {
                codec.releaseOutputBuffer(index, false)
            }
        }
    }

    /** Encode a key frame as soon as possible (called from the control thread). */
    fun requestKeyFrame() {
        val current = codec ?: return
        runCatching {
            current.setParameters(Bundle().apply { putInt(MediaCodec.PARAMETER_KEY_REQUEST_SYNC_FRAME, 0) })
        }
    }

    private fun writeSession(frame: DisplaySize) {
        out.writeLong(1L shl 63)
        out.writeInt(8)
        out.writeInt(frame.width)
        out.writeInt(frame.height)
        out.flush()
    }

    private fun startRotationWatcher() {
        Thread({
            while (true) {
                Thread.sleep(300)
                val now = runCatching { Displays.mainDisplaySize() }.getOrNull() ?: continue
                if (now != displaySize) resetRequested = true
            }
        }, "sentinel-rotation").apply { isDaemon = true }.start()
    }

    private fun format(frame: DisplaySize) =
        MediaFormat.createVideoFormat(MediaFormat.MIMETYPE_VIDEO_AVC, frame.width, frame.height).apply {
            setInteger(MediaFormat.KEY_COLOR_FORMAT, MediaCodecInfo.CodecCapabilities.COLOR_FormatSurface)
            setInteger(MediaFormat.KEY_BIT_RATE, bitRate)
            setInteger(MediaFormat.KEY_FRAME_RATE, 60)
            setInteger(MediaFormat.KEY_I_FRAME_INTERVAL, 10)
            // Keep sending frames on a static screen so the desktop always has a current picture.
            setLong(MediaFormat.KEY_REPEAT_PREVIOUS_FRAME_AFTER, 100_000)
            if (maxFps > 0) setFloat("max-fps-to-encoder", maxFps.toFloat())
        }

    /** Fit within [maxSize] on the longest side, keeping dimensions multiples of 8 for encoders. */
    private fun scaled(display: DisplaySize): DisplaySize {
        val longest = maxOf(display.width, display.height)
        val scale = if (maxSize in 1 until longest) maxSize.toDouble() / longest else 1.0
        fun align(v: Double) = maxOf(8, (Math.round(v / 8) * 8).toInt())
        return DisplaySize(align(display.width * scale), align(display.height * scale))
    }
}
