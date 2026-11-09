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
// control socket (desktop -> agent), see Input.kt for message layouts. Not opened with
// `control=0` (view only).
//
// Only connections from adbd (the shell or root user) are accepted; see acceptFromAdb.
//
// With `mode=apps` the agent instead prints the installed apps to stdout and exits (Apps.kt).
package dev.sentinel.agent

import android.net.LocalServerSocket
import android.net.LocalSocket
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
        // control=0: view only; the desktop never opens the control socket.
        val control = opts["control"] != "0"

        Workarounds.apply()

        try {
            run(socketName, maxSize, bitRate, maxFps, control)
        } catch (e: Throwable) {
            log("fatal: $e")
            e.printStackTrace()
            exitProcess(1)
        }
        exitProcess(0)
    }

    private fun run(socketName: String, maxSize: Int, bitRate: Int, maxFps: Int, control: Boolean) {
        val server = LocalServerSocket(socketName)
        log("listening on @$socketName")

        // The desktop connects the video socket first and waits for the handshake,
        // then (unless view-only) connects the control socket.
        val videoSocket = acceptFromAdb(server)
        val video = DataOutputStream(BufferedOutputStream(videoSocket.outputStream, 1 shl 16))
        val name = (android.os.Build.MODEL ?: "Android").toByteArray(Charsets.UTF_8)
        video.write("SNTL".toByteArray(Charsets.US_ASCII))
        video.writeByte(PROTOCOL_VERSION)
        video.writeShort(name.size)
        video.write(name)
        video.flush()

        val controlSocket = if (control) acceptFromAdb(server) else null
        server.close()

        val streamer = ScreenStreamer(video, maxSize, bitRate, maxFps)

        // When the desktop goes away either socket fails; then we're done.
        if (controlSocket != null) {
            val input = InputHandler(streamer)
            Thread({
                try {
                    input.serve(controlSocket.inputStream)
                } catch (e: Exception) {
                    log("control closed: $e")
                }
                exitProcess(0)
            }, "sentinel-control").start()
        }

        streamer.run() // returns only when the video socket fails
    }

    /**
     * Accept the next connection that comes through `adb forward`. The socket lives in the
     * abstract namespace, which has no file permissions, so any app on the phone could connect;
     * only adbd (running as shell, or root after `adb root`) may watch the screen or send input.
     */
    private fun acceptFromAdb(server: LocalServerSocket): LocalSocket {
        while (true) {
            val socket = server.accept()
            val uid = runCatching { socket.peerCredentials.uid }.getOrDefault(-1)
            if (uid == SHELL_UID || uid == ROOT_UID) return socket
            log("rejected a connection from uid $uid")
            runCatching { socket.close() }
        }
    }

    private const val SHELL_UID = 2000
    private const val ROOT_UID = 0
}
