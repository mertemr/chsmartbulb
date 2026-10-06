package io.github.mertemr.chsmartbulb.bluetooth

import android.app.Notification
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.app.Service
import android.content.Context
import android.content.Intent
import android.content.pm.ServiceInfo
import android.os.Build
import android.os.IBinder

private const val CHANNEL_ID = "bulb"
private const val NOTIFICATION_ID = 1

/**
 * A foreground service with a quiet notification. While it runs Android keeps the app
 * alive, so the link stays up and effects keep playing with the screen off. With
 * `projection` it also covers capturing the phone's audio, which Android 14 requires.
 */
class KeepAliveService : Service() {
    override fun onBind(intent: Intent?): IBinder? = null

    override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int {
        val title = intent?.getStringExtra("title")?.takeIf { it.isNotBlank() } ?: "Driving the bulb"
        val projection = intent?.getBooleanExtra("projection", false) == true
        val notification = notification(title, projection)
        if (Build.VERSION.SDK_INT >= 29) {
            var types = ServiceInfo.FOREGROUND_SERVICE_TYPE_CONNECTED_DEVICE
            if (projection) types = types or ServiceInfo.FOREGROUND_SERVICE_TYPE_MEDIA_PROJECTION
            startForeground(NOTIFICATION_ID, notification, types)
        } else {
            startForeground(NOTIFICATION_ID, notification)
        }
        return START_NOT_STICKY
    }

    private fun notification(title: String, projection: Boolean): Notification {
        val manager = getSystemService(Context.NOTIFICATION_SERVICE) as NotificationManager
        if (Build.VERSION.SDK_INT >= 26 && manager.getNotificationChannel(CHANNEL_ID) == null) {
            val channel = NotificationChannel(CHANNEL_ID, "Bulb connection", NotificationManager.IMPORTANCE_LOW)
            channel.description = "Shown while the app keeps the bulb connected"
            manager.createNotificationChannel(channel)
        }
        val launch = packageManager.getLaunchIntentForPackage(packageName)
        val open = launch?.let {
            PendingIntent.getActivity(this, 0, it, PendingIntent.FLAG_IMMUTABLE or PendingIntent.FLAG_UPDATE_CURRENT)
        }
        val text = if (projection) "Following this phone's sound" else "Keeping the bulb connected"
        val builder = if (Build.VERSION.SDK_INT >= 26) Notification.Builder(this, CHANNEL_ID) else {
            @Suppress("DEPRECATION")
            Notification.Builder(this)
        }
        return builder
            .setContentTitle(title)
            .setContentText(text)
            .setSmallIcon(android.R.drawable.stat_sys_data_bluetooth)
            .setOngoing(true)
            .setContentIntent(open)
            .build()
    }

    companion object {
        @Volatile
        var running = false
            private set
        @Volatile
        var lastTitle = ""
            private set

        fun start(context: Context, title: String, projection: Boolean) {
            lastTitle = title
            val intent = Intent(context, KeepAliveService::class.java)
                .putExtra("title", title)
                .putExtra("projection", projection)
            if (Build.VERSION.SDK_INT >= 26) context.startForegroundService(intent) else context.startService(intent)
            running = true
        }

        fun stop(context: Context) {
            if (!running) return
            context.stopService(Intent(context, KeepAliveService::class.java))
            running = false
        }
    }
}
