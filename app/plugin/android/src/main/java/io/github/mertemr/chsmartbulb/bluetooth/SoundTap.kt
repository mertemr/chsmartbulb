package io.github.mertemr.chsmartbulb.bluetooth

import android.annotation.SuppressLint
import android.media.AudioAttributes
import android.media.AudioFormat
import android.media.AudioPlaybackCaptureConfiguration
import android.media.AudioRecord
import android.media.MediaRecorder
import android.media.projection.MediaProjection
import android.util.Base64
import androidx.annotation.RequiresApi
import app.tauri.plugin.Channel
import app.tauri.plugin.JSObject

private const val RATE = 44100
/**
 * What reaches the Rust side: every two frames averaged into one. The analysis looks at
 * nothing above 8 kHz, so half the rate keeps all it needs and halves what crosses over.
 */
private const val SENT_RATE = RATE / 2
/** Frames read at a time: about 23 ms, one analysis block on the Rust side once halved. */
private const val FRAMES = 1024

/** Average each pair of 16-bit little-endian frames of `size` bytes into one. */
internal fun halve(buffer: ByteArray, size: Int, channels: Int): ByteArray {
    val frame = channels * 2
    val pairs = size / (2 * frame)
    val out = ByteArray(pairs * frame)
    for (pair in 0 until pairs) {
        for (channel in 0 until channels) {
            val first = 2 * pair * frame + channel * 2
            val second = first + frame
            val a = (buffer[first].toInt() and 0xff) or (buffer[first + 1].toInt() shl 8)
            val b = (buffer[second].toInt() and 0xff) or (buffer[second + 1].toInt() shl 8)
            val mean = (a + b) shr 1
            val at = pair * frame + channel * 2
            out[at] = mean.toByte()
            out[at + 1] = (mean shr 8).toByte()
        }
    }
    return out
}

/**
 * Hands 16-bit PCM to the Rust side, which turns it into band levels and beats.
 * Only the analysis results ever reach the bulb; the sound itself stays on the phone.
 */
class SoundTap private constructor(
    private val record: AudioRecord,
    private val channels: Int,
    private val channel: Channel,
    private val projection: MediaProjection?,
) {
    @Volatile
    private var running = false
    private var thread: Thread? = null

    val isPlayback: Boolean get() = projection != null

    fun start() {
        running = true
        record.startRecording()
        thread = Thread {
            val buffer = ByteArray(FRAMES * channels * 2)
            while (running) {
                val size = record.read(buffer, 0, buffer.size)
                if (size <= 0) continue
                val event = JSObject()
                event.put("event", "pcm")
                event.put("rate", SENT_RATE)
                event.put("channels", channels)
                event.put("data", Base64.encodeToString(halve(buffer, size, channels), Base64.NO_WRAP))
                channel.send(event)
            }
        }.also { it.start() }
    }

    fun stop() {
        running = false
        try { record.stop() } catch (ignored: IllegalStateException) {}
        thread?.join(500)
        record.release()
        projection?.stop()
    }

    companion object {
        private fun bufferSize(channelMask: Int, channels: Int): Int =
            maxOf(AudioRecord.getMinBufferSize(RATE, channelMask, AudioFormat.ENCODING_PCM_16BIT), FRAMES * channels * 4)

        /** The room, through the microphone. */
        @SuppressLint("MissingPermission")
        fun microphone(channel: Channel): SoundTap {
            val mask = AudioFormat.CHANNEL_IN_MONO
            val record = AudioRecord(MediaRecorder.AudioSource.MIC, RATE, mask, AudioFormat.ENCODING_PCM_16BIT, bufferSize(mask, 1))
            if (record.state != AudioRecord.STATE_INITIALIZED) {
                record.release()
                throw IllegalStateException("The microphone is not available")
            }
            return SoundTap(record, 1, channel, null)
        }

        /** What this phone plays, wherever it plays it: its speaker, headphones, any Bluetooth speaker. */
        @SuppressLint("MissingPermission")
        @RequiresApi(29)
        fun playback(projection: MediaProjection, channel: Channel): SoundTap {
            val config = AudioPlaybackCaptureConfiguration.Builder(projection)
                .addMatchingUsage(AudioAttributes.USAGE_MEDIA)
                .addMatchingUsage(AudioAttributes.USAGE_GAME)
                .addMatchingUsage(AudioAttributes.USAGE_UNKNOWN)
                .build()
            val mask = AudioFormat.CHANNEL_IN_STEREO
            val format = AudioFormat.Builder()
                .setEncoding(AudioFormat.ENCODING_PCM_16BIT)
                .setSampleRate(RATE)
                .setChannelMask(mask)
                .build()
            val record = AudioRecord.Builder()
                .setAudioFormat(format)
                .setBufferSizeInBytes(bufferSize(mask, 2))
                .setAudioPlaybackCaptureConfig(config)
                .build()
            if (record.state != AudioRecord.STATE_INITIALIZED) {
                record.release()
                projection.stop()
                throw IllegalStateException("This phone's audio cannot be captured")
            }
            return SoundTap(record, 2, channel, projection)
        }
    }
}
