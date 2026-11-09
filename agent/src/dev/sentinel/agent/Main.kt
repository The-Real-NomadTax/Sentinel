// Sentinel's on-device agent. The desktop app pushes it to /data/local/tmp and starts it with
// `app_process` as the shell user. It streams the screen as H.264 and injects input.
//
// Wire protocol (all integers big-endian), over two connections to a local abstract socket:
//
// video socket (agent -> desktop)
//   handshake: "SNTL" | version u8 | name length u16 | device name (UTF-8)
//   packets:   flags+pts u64 | length u32 | payload
//              bit 63 = session (payload: width u32, height u32; a new stream starts)
//              bit 62 = codec config (SPS/PPS)
//              bit 61 = key frame
//              low 61 bits = presentation time in microseconds
//
// control socket (desktop -> agent), see Input.kt for message layouts.
//
// With `mode=apps` the agent instead prints the installed apps to stdout and exits (Apps.kt).
package dev.sentinel.agent

import android.net.LocalServerSocket
import java.io.BufferedOutputStream
import java.io.DataOutputStream
import java.io.File
import kotlin.system.exitProcess

const val PROTOCOL_VERSION = 1

fun log(msg: String) = System.err.println("[agent] $msg")

object Main {
    @JvmStatic
    fun main(args: Array<String>) {
        val opts = args.mapNotNull { arg ->
            val parts = arg.split("=", limit = 2)
            if (parts.size == 2) parts[0] to parts[1] else null
        }.toMap()

        // Delete our own jar: the classes are already loaded, and nothing is left behind.
        opts["cleanup"]?.let { File(it).delete() }

        if (opts["mode"] == "apps") {
            Workarounds.apply()
            try {
                Apps.list()
            } catch (e: Throwable) {
                log("fatal: $e")
                e.printStackTrace()
                exitProcess(1)
            }
            exitProcess(0)
        }

        val socketName = opts["socket"] ?: run {
            log("missing socket=<name>")
            exitProcess(2)
        }
        val maxSize = opts["max_size"]?.toIntOrNull() ?: 1280
        val bitRate = opts["bit_rate"]?.toIntOrNull() ?: 8_000_000
        val maxFps = opts["max_fps"]?.toIntOrNull() ?: 60

        Workarounds.apply()

        try {
            run(socketName, maxSize, bitRate, maxFps)
        } catch (e: Throwable) {
            log("fatal: $e")
            e.printStackTrace()
            exitProcess(1)
        }
        exitProcess(0)
    }

    private fun run(socketName: String, maxSize: Int, bitRate: Int, maxFps: Int) {
        val server = LocalServerSocket(socketName)
        log("listening on @$socketName")

        // The desktop connects the video socket first and waits for the handshake,
        // then connects the control socket.
        val videoSocket = server.accept()
        val video = DataOutputStream(BufferedOutputStream(videoSocket.outputStream, 1 shl 16))
        val name = (android.os.Build.MODEL ?: "Android").toByteArray(Charsets.UTF_8)
        video.write("SNTL".toByteArray(Charsets.US_ASCII))
        video.writeByte(PROTOCOL_VERSION)
        video.writeShort(name.size)
        video.write(name)
        video.flush()

        val controlSocket = server.accept()
        server.close()

        val streamer = ScreenStreamer(video, maxSize, bitRate, maxFps)
        val input = InputHandler(streamer)

        // When the desktop goes away either socket fails; then we're done.
        Thread({
            try {
                input.serve(controlSocket.inputStream)
            } catch (e: Exception) {
                log("control closed: $e")
            }
            exitProcess(0)
        }, "sentinel-control").start()

        streamer.run() // returns only when the video socket fails
    }
}
