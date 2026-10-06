package io.github.mertemr.chsmartbulb.bluetooth

import android.Manifest
import android.annotation.SuppressLint
import android.app.Activity
import android.bluetooth.BluetoothAdapter
import android.bluetooth.BluetoothDevice
import android.bluetooth.BluetoothManager
import android.bluetooth.BluetoothProfile
import android.bluetooth.le.ScanCallback
import android.bluetooth.le.ScanResult
import android.bluetooth.le.ScanSettings
import android.content.Context
import android.content.Intent
import android.media.AudioDeviceInfo
import android.media.AudioManager
import android.media.projection.MediaProjectionManager
import android.os.Build
import android.os.Handler
import android.os.Looper
import android.provider.Settings
import android.util.Base64
import androidx.activity.result.ActivityResult
import app.tauri.PermissionState
import app.tauri.annotation.ActivityCallback
import app.tauri.annotation.Command
import app.tauri.annotation.InvokeArg
import app.tauri.annotation.Permission
import app.tauri.annotation.PermissionCallback
import app.tauri.annotation.TauriPlugin
import app.tauri.plugin.Channel
import app.tauri.plugin.Invoke
import app.tauri.plugin.JSArray
import app.tauri.plugin.JSObject
import app.tauri.plugin.Plugin

@InvokeArg
class ScanArgs {
    var seconds: Double = 4.0
}

@InvokeArg
class ConnectArgs {
    var address: String = ""
    var bearer: String = "ble"
    lateinit var onEvent: Channel
}

@InvokeArg
class WriteArgs {
    var data: String = ""
}

@InvokeArg
class AudioArgs {
    var input: String = "playback"
    lateinit var onEvent: Channel
}

@InvokeArg
class SettingsArgs {
    var which: String = "bluetooth"
}

@InvokeArg
class KeepRunningArgs {
    var running: Boolean = false
    var title: String = ""
}

/** Company id in the bulb's LE manufacturer data (CUBE Technologies). */
private const val BULB_COMPANY_ID = 0x03EE
private val BULB_NAMES = listOf("SmartBulb Bluetooth", "Chsmartbulb")

/**
 * The bulb's Bluetooth links and the phone's sound, for the Rust side of the app.
 *
 * One link is open at a time, over BLE (GATT) or Bluetooth Classic (SPP). Received
 * bytes and the end of the link go to the Rust side through the channel given to
 * `connect`; captured sound goes through the channel given to `startAudio`.
 */
@SuppressLint("MissingPermission") // every Bluetooth call follows `prepare`, which asks for the permissions
@TauriPlugin(
    permissions = [
        Permission(strings = [Manifest.permission.BLUETOOTH_SCAN, Manifest.permission.BLUETOOTH_CONNECT], alias = "bluetooth"),
        Permission(strings = [Manifest.permission.ACCESS_FINE_LOCATION], alias = "location"),
        Permission(strings = [Manifest.permission.RECORD_AUDIO], alias = "microphone"),
        Permission(strings = [Manifest.permission.POST_NOTIFICATIONS], alias = "notifications"),
    ]
)
class BluetoothPlugin(private val activity: Activity) : Plugin(activity) {
    private val main = Handler(Looper.getMainLooper())
    private var link: BulbLink? = null
    private var tap: SoundTap? = null
    private var pendingAudio: AudioArgs? = null

    private fun adapter(): BluetoothAdapter? =
        (activity.getSystemService(Context.BLUETOOTH_SERVICE) as BluetoothManager?)?.adapter

    private fun bluetoothAlias(): String = if (Build.VERSION.SDK_INT >= 31) "bluetooth" else "location"

    // --- readiness ------------------------------------------------------------------

    /** Ask for the Bluetooth permissions, then to switch Bluetooth on. */
    @Command
    fun prepare(invoke: Invoke) {
        if (getPermissionState(bluetoothAlias()) != PermissionState.GRANTED) {
            requestPermissionForAlias(bluetoothAlias(), invoke, "prepared")
        } else {
            enableBluetooth(invoke)
        }
    }

    @PermissionCallback
    private fun prepared(invoke: Invoke) {
        if (getPermissionState(bluetoothAlias()) != PermissionState.GRANTED) {
            invoke.resolve(readiness(granted = false))
            return
        }
        if (Build.VERSION.SDK_INT >= 33 && getPermissionState("notifications") != PermissionState.GRANTED) {
            requestPermissionForAlias("notifications", invoke, "notified")
            return
        }
        enableBluetooth(invoke)
    }

    @PermissionCallback
    private fun notified(invoke: Invoke) {
        enableBluetooth(invoke) // the notification only keeps effects running with the screen off
    }

    @SuppressLint("MissingPermission")
    private fun enableBluetooth(invoke: Invoke) {
        val adapter = adapter()
        if (adapter == null) {
            invoke.reject("This device has no Bluetooth")
            return
        }
        if (adapter.isEnabled) {
            invoke.resolve(readiness(granted = true))
            return
        }
        main.post { startActivityForResult(invoke, Intent(BluetoothAdapter.ACTION_REQUEST_ENABLE), "enabled") }
    }

    @ActivityCallback
    private fun enabled(invoke: Invoke, @Suppress("UNUSED_PARAMETER") result: ActivityResult) {
        invoke.resolve(readiness(granted = true))
    }

    private fun readiness(granted: Boolean): JSObject {
        val ready = JSObject()
        ready.put("granted", granted)
        ready.put("enabled", adapter()?.isEnabled == true)
        return ready
    }

    // --- finding the bulb ------------------------------------------------------------

    /** Bonded devices, then whatever advertises over BLE for `seconds`. */
    @SuppressLint("MissingPermission")
    @Command
    fun scan(invoke: Invoke) {
        val args = invoke.parseArgs(ScanArgs::class.java)
        val adapter = adapter()
        if (adapter == null || !adapter.isEnabled) {
            invoke.reject("Bluetooth is off")
            return
        }
        val found = LinkedHashMap<String, JSObject>()
        try {
            for (device in adapter.bondedDevices.orEmpty()) {
                found[device.address] = describe(device, null, null, bonded = true)
            }
        } catch (e: SecurityException) {
            invoke.reject("Bluetooth permission was not granted")
            return
        }
        val scanner = adapter.bluetoothLeScanner
        if (scanner == null) {
            invoke.resolve(devices(found))
            return
        }
        val callback = object : ScanCallback() {
            override fun onScanResult(callbackType: Int, result: ScanResult) {
                val record = result.scanRecord
                val companies = mutableListOf<Int>()
                record?.manufacturerSpecificData?.let { data ->
                    for (i in 0 until data.size()) companies.add(data.keyAt(i))
                }
                val known = found[result.device.address]
                val entry = describe(result.device, record?.deviceName, result.rssi, bonded = known?.getBoolean("bonded") == true, companies = companies)
                entry.put("le", true)
                synchronized(found) { found[result.device.address] = entry }
            }
        }
        val settings = ScanSettings.Builder().setScanMode(ScanSettings.SCAN_MODE_LOW_LATENCY).build()
        try {
            scanner.startScan(null, settings, callback)
        } catch (e: SecurityException) {
            invoke.reject("Bluetooth permission was not granted")
            return
        }
        main.postDelayed({
            try {
                scanner.stopScan(callback)
            } catch (e: Exception) {
                // Bluetooth went off meanwhile; report what was seen
            }
            synchronized(found) { invoke.resolve(devices(found)) }
        }, (args.seconds.coerceIn(0.5, 30.0) * 1000).toLong())
    }

    @SuppressLint("MissingPermission")
    private fun describe(
        device: BluetoothDevice,
        advertisedName: String?,
        rssi: Int?,
        bonded: Boolean,
        companies: List<Int> = emptyList(),
    ): JSObject {
        val name = advertisedName ?: try { device.name } catch (e: SecurityException) { null }
        val type = try { device.type } catch (e: SecurityException) { BluetoothDevice.DEVICE_TYPE_UNKNOWN }
        val entry = JSObject()
        entry.put("address", device.address)
        entry.put("name", name)
        if (rssi != null) entry.put("rssi", rssi)
        entry.put("bonded", bonded)
        entry.put("classic", type == BluetoothDevice.DEVICE_TYPE_CLASSIC || type == BluetoothDevice.DEVICE_TYPE_DUAL)
        entry.put("le", type == BluetoothDevice.DEVICE_TYPE_LE || type == BluetoothDevice.DEVICE_TYPE_DUAL)
        val likely = BULB_NAMES.any { it.equals(name, ignoreCase = true) } ||
            companies.contains(BULB_COMPANY_ID) ||
            device.address.uppercase().startsWith("F4:4E:FD")
        entry.put("likely", likely)
        return entry
    }

    private fun devices(found: Map<String, JSObject>): JSObject {
        val list = JSArray()
        for (entry in found.values) list.put(entry)
        val result = JSObject()
        result.put("devices", list)
        return result
    }

    // --- the link --------------------------------------------------------------------

    @Command
    fun connect(invoke: Invoke) {
        val args = invoke.parseArgs(ConnectArgs::class.java)
        val adapter = adapter()
        if (adapter == null || !adapter.isEnabled) {
            invoke.reject("Bluetooth is off")
            return
        }
        link?.close()
        val device = try {
            adapter.getRemoteDevice(args.address.uppercase())
        } catch (e: IllegalArgumentException) {
            invoke.reject("Not a Bluetooth address: ${args.address}")
            return
        }
        val events = LinkEvents(args.onEvent)
        val opened = if (args.bearer == "spp") SppLink(adapter, device, events) else GattLink(activity, device, events)
        link = opened
        opened.open(
            onOpen = { invoke.resolve() },
            onFail = { message ->
                if (link === opened) link = null
                invoke.reject(message)
            },
        )
    }

    @Command
    fun write(invoke: Invoke) {
        val args = invoke.parseArgs(WriteArgs::class.java)
        val current = link
        if (current == null) {
            invoke.reject("not connected")
            return
        }
        current.write(Base64.decode(args.data, Base64.NO_WRAP), onDone = { invoke.resolve() }, onFail = { invoke.reject(it) })
    }

    @Command
    fun disconnect(invoke: Invoke) {
        link?.close()
        link = null
        invoke.resolve()
    }

    // --- sound -----------------------------------------------------------------------

    /**
     * Start listening for the sound-reactive effects. `playback` captures what this
     * phone plays (Android 10+, after the user agrees to share the audio once per
     * session); `microphone` hears the room.
     */
    @Command
    fun startAudio(invoke: Invoke) {
        val args = invoke.parseArgs(AudioArgs::class.java)
        if (getPermissionState("microphone") != PermissionState.GRANTED) {
            pendingAudio = args
            requestPermissionForAlias("microphone", invoke, "microphoneGranted")
            return
        }
        beginAudio(invoke, args)
    }

    @PermissionCallback
    private fun microphoneGranted(invoke: Invoke) {
        val args = pendingAudio ?: invoke.parseArgs(AudioArgs::class.java)
        pendingAudio = null
        if (getPermissionState("microphone") != PermissionState.GRANTED) {
            invoke.reject("Recording permission was not granted")
            return
        }
        beginAudio(invoke, args)
    }

    private fun beginAudio(invoke: Invoke, args: AudioArgs) {
        tap?.stop()
        tap = null
        if (args.input == "playback" && Build.VERSION.SDK_INT >= 29) {
            pendingAudio = args
            val manager = activity.getSystemService(Context.MEDIA_PROJECTION_SERVICE) as MediaProjectionManager
            main.post { startActivityForResult(invoke, manager.createScreenCaptureIntent(), "projectionGranted") }
            return
        }
        try {
            tap = SoundTap.microphone(args.onEvent).also { it.start() }
            invoke.resolve()
        } catch (e: Exception) {
            invoke.reject(e.message ?: e.toString())
        }
    }

    @ActivityCallback
    private fun projectionGranted(invoke: Invoke, result: ActivityResult) {
        val args = pendingAudio
        pendingAudio = null
        val data = result.data
        if (args == null || result.resultCode != Activity.RESULT_OK || data == null) {
            invoke.reject("Sharing this phone's audio was declined")
            return
        }
        // Android 14 wants a media projection service running before the projection is used.
        KeepAliveService.start(activity, KeepAliveService.lastTitle, projection = true)
        main.postDelayed({
            try {
                if (Build.VERSION.SDK_INT < 29) throw IllegalStateException("Capturing playback needs Android 10")
                val manager = activity.getSystemService(Context.MEDIA_PROJECTION_SERVICE) as MediaProjectionManager
                val projection = manager.getMediaProjection(result.resultCode, data)
                    ?: throw IllegalStateException("Android did not hand over the audio")
                tap = SoundTap.playback(projection, args.onEvent).also { it.start() }
                invoke.resolve()
            } catch (e: Exception) {
                invoke.reject(e.message ?: e.toString())
            }
        }, 300)
    }

    @Command
    fun stopAudio(invoke: Invoke) {
        tap?.stop()
        tap = null
        if (KeepAliveService.running) KeepAliveService.start(activity, KeepAliveService.lastTitle, projection = false)
        invoke.resolve()
    }

    /** Where the sound goes now: outputs in use and Bluetooth devices connected as speakers. */
    @SuppressLint("MissingPermission")
    @Command
    fun audioRoute(invoke: Invoke) {
        val audio = activity.getSystemService(Context.AUDIO_SERVICE) as AudioManager
        val outputs = JSArray()
        for (device in audio.getDevices(AudioManager.GET_DEVICES_OUTPUTS)) {
            val label = when (device.type) {
                AudioDeviceInfo.TYPE_BUILTIN_SPEAKER -> "Phone speaker"
                AudioDeviceInfo.TYPE_BLUETOOTH_A2DP -> "Bluetooth: ${device.productName}"
                AudioDeviceInfo.TYPE_WIRED_HEADPHONES, AudioDeviceInfo.TYPE_WIRED_HEADSET -> "Headphones"
                AudioDeviceInfo.TYPE_USB_HEADSET, AudioDeviceInfo.TYPE_USB_DEVICE -> "USB: ${device.productName}"
                else -> null
            }
            if (label != null) outputs.put(label)
        }
        val result = JSObject()
        result.put("outputs", outputs)
        result.put("canCapturePlayback", Build.VERSION.SDK_INT >= 29)
        val speakers = JSArray()
        val adapter = adapter()
        if (adapter == null || !adapter.isEnabled) {
            result.put("speakers", speakers)
            invoke.resolve(result)
            return
        }
        val answered = adapter.getProfileProxy(activity, object : BluetoothProfile.ServiceListener {
            override fun onServiceConnected(profile: Int, proxy: BluetoothProfile) {
                try {
                    for (device in proxy.connectedDevices) speakers.put(device.address)
                } catch (e: SecurityException) {
                    // no permission yet: nothing to list
                }
                adapter.closeProfileProxy(profile, proxy)
                result.put("speakers", speakers)
                invoke.resolve(result)
            }

            override fun onServiceDisconnected(profile: Int) {}
        }, BluetoothProfile.A2DP)
        if (!answered) {
            result.put("speakers", speakers)
            invoke.resolve(result)
        }
    }

    @Command
    fun openSettings(invoke: Invoke) {
        val args = invoke.parseArgs(SettingsArgs::class.java)
        val action = when (args.which) {
            "sound" -> Settings.ACTION_SOUND_SETTINGS
            else -> Settings.ACTION_BLUETOOTH_SETTINGS
        }
        main.post {
            try {
                activity.startActivity(Intent(action))
            } catch (e: Exception) {
                activity.startActivity(Intent(Settings.ACTION_SETTINGS))
            }
        }
        invoke.resolve()
    }

    /** A notification that keeps the app alive while it drives the bulb with the screen off. */
    @Command
    fun keepRunning(invoke: Invoke) {
        val args = invoke.parseArgs(KeepRunningArgs::class.java)
        try {
            if (args.running) {
                KeepAliveService.start(activity, args.title, projection = tap?.isPlayback == true)
            } else {
                KeepAliveService.stop(activity)
            }
            invoke.resolve()
        } catch (e: Exception) {
            invoke.reject(e.message ?: e.toString())
        }
    }

    override fun onDestroy() {
        tap?.stop()
        link?.close()
        KeepAliveService.stop(activity)
        super.onDestroy()
    }
}
