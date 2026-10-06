package io.github.mertemr.chsmartbulb.bluetooth

import android.annotation.SuppressLint
import android.bluetooth.BluetoothAdapter
import android.bluetooth.BluetoothDevice
import android.bluetooth.BluetoothGatt
import android.bluetooth.BluetoothGattCallback
import android.bluetooth.BluetoothGattCharacteristic
import android.bluetooth.BluetoothGattDescriptor
import android.bluetooth.BluetoothProfile
import android.bluetooth.BluetoothSocket
import android.content.Context
import android.os.Build
import android.util.Base64
import app.tauri.plugin.Channel
import app.tauri.plugin.JSObject
import java.io.IOException
import java.util.UUID
import java.util.concurrent.Executors

private val SPP_UUID: UUID = UUID.fromString("00001101-0000-1000-8000-00805f9b34fb")
private val COMMAND_SERVICE: UUID = UUID.fromString("00007777-0000-1000-8000-00805f9b34fb")
private val COMMAND: UUID = UUID.fromString("00008877-0000-1000-8000-00805f9b34fb")
private val ANSWER_SERVICE: UUID = UUID.fromString("00006666-0000-1000-8000-00805f9b34fb")
private val ANSWER: UUID = UUID.fromString("00008888-0000-1000-8000-00805f9b34fb")
private val CLIENT_CONFIG: UUID = UUID.fromString("00002902-0000-1000-8000-00805f9b34fb")
private const val RFCOMM_CHANNEL = 2

/** Tells the Rust side what arrives on a link and when it ends. */
class LinkEvents(private val channel: Channel) {
    @Volatile
    private var ended = false

    fun data(bytes: ByteArray) {
        val event = JSObject()
        event.put("event", "data")
        event.put("data", Base64.encodeToString(bytes, Base64.NO_WRAP))
        channel.send(event)
    }

    fun closed(reason: String) {
        if (ended) return
        ended = true
        val event = JSObject()
        event.put("event", "closed")
        event.put("reason", reason)
        channel.send(event)
    }
}

/** An open pipe to the bulb. The Rust side sends one write at a time and waits for it. */
interface BulbLink {
    fun open(onOpen: () -> Unit, onFail: (String) -> Unit)
    fun write(data: ByteArray, onDone: () -> Unit, onFail: (String) -> Unit)
    fun close()
}

/**
 * Bluetooth Classic serial port. Works while the bulb is also this phone's speaker,
 * and needs the bulb to be paired.
 */
@SuppressLint("MissingPermission")
class SppLink(
    private val adapter: BluetoothAdapter,
    private val device: BluetoothDevice,
    private val events: LinkEvents,
) : BulbLink {
    private val writer = Executors.newSingleThreadExecutor()
    @Volatile
    private var socket: BluetoothSocket? = null

    override fun open(onOpen: () -> Unit, onFail: (String) -> Unit) {
        Thread {
            try {
                adapter.cancelDiscovery()
                val opened = connectSocket()
                socket = opened
                onOpen()
                read(opened)
            } catch (e: Exception) {
                onFail("Cannot connect to ${device.address} over SPP: ${e.message ?: e}")
            }
        }.start()
    }

    private fun connectSocket(): BluetoothSocket {
        val first = device.createRfcommSocketToServiceRecord(SPP_UUID)
        try {
            first.connect()
            return first
        } catch (e: IOException) {
            try { first.close() } catch (ignored: IOException) {}
            // Some stacks fail the service lookup; the bulb's serial port is always on channel 2.
            val method = device.javaClass.getMethod("createRfcommSocket", Int::class.javaPrimitiveType)
            val second = method.invoke(device, RFCOMM_CHANNEL) as BluetoothSocket
            second.connect()
            return second
        }
    }

    private fun read(opened: BluetoothSocket) {
        val buffer = ByteArray(1024)
        try {
            val input = opened.inputStream
            while (true) {
                val size = input.read(buffer)
                if (size < 0) break
                if (size > 0) events.data(buffer.copyOf(size))
            }
            events.closed("closed by the device")
        } catch (e: IOException) {
            events.closed(e.message ?: "read failed")
        }
    }

    override fun write(data: ByteArray, onDone: () -> Unit, onFail: (String) -> Unit) {
        writer.execute {
            val current = socket
            if (current == null) {
                onFail("SPP link is closed")
                return@execute
            }
            try {
                current.outputStream.write(data)
                current.outputStream.flush()
                onDone()
            } catch (e: IOException) {
                events.closed(e.message ?: "write failed")
                onFail("SPP write failed: ${e.message}")
            }
        }
    }

    override fun close() {
        val current = socket
        socket = null
        try { current?.close() } catch (ignored: IOException) {}
        writer.shutdown()
        events.closed("closed")
    }
}

/**
 * BLE: frames are written with response to 0x8877 and answered through
 * notifications on 0x8888. Only available while no Classic link to the bulb is up.
 */
@SuppressLint("MissingPermission")
class GattLink(
    private val context: Context,
    private val device: BluetoothDevice,
    private val events: LinkEvents,
) : BulbLink {
    private var gatt: BluetoothGatt? = null
    private var command: BluetoothGattCharacteristic? = null
    private var opening: Pair<() -> Unit, (String) -> Unit>? = null
    private var writing: Pair<() -> Unit, (String) -> Unit>? = null
    private val lock = Object()

    override fun open(onOpen: () -> Unit, onFail: (String) -> Unit) {
        synchronized(lock) { opening = Pair(onOpen, onFail) }
        gatt = if (Build.VERSION.SDK_INT >= 23) {
            device.connectGatt(context, false, callback, BluetoothDevice.TRANSPORT_LE)
        } else {
            device.connectGatt(context, false, callback)
        }
    }

    private fun failOpening(message: String) {
        val pending = synchronized(lock) { opening.also { opening = null } }
        if (pending != null) {
            pending.second(message)
            gatt?.close()
            gatt = null
        }
    }

    private val callback = object : BluetoothGattCallback() {
        override fun onConnectionStateChange(gatt: BluetoothGatt, status: Int, newState: Int) {
            if (newState == BluetoothProfile.STATE_CONNECTED) {
                // larger packets so a long answer needs fewer notifications
                if (!gatt.requestMtu(185)) gatt.discoverServices()
                return
            }
            if (newState == BluetoothProfile.STATE_DISCONNECTED) {
                failOpening("Cannot connect to ${device.address} over BLE (status $status). " +
                    "While the bulb is connected over Classic, as a speaker, it does not take BLE.")
                val pending = synchronized(lock) { writing.also { writing = null } }
                pending?.second?.invoke("BLE link lost")
                events.closed("BLE link lost (status $status)")
                gatt.close()
            }
        }

        override fun onMtuChanged(gatt: BluetoothGatt, mtu: Int, status: Int) {
            gatt.discoverServices()
        }

        override fun onServicesDiscovered(gatt: BluetoothGatt, status: Int) {
            val commandCharacteristic = gatt.getService(COMMAND_SERVICE)?.getCharacteristic(COMMAND)
            val answer = gatt.getService(ANSWER_SERVICE)?.getCharacteristic(ANSWER)
            if (status != BluetoothGatt.GATT_SUCCESS || commandCharacteristic == null || answer == null) {
                failOpening("${device.address} does not offer the bulb's BLE service; is it the bulb?")
                gatt.disconnect()
                return
            }
            command = commandCharacteristic
            gatt.setCharacteristicNotification(answer, true)
            val config = answer.getDescriptor(CLIENT_CONFIG)
            if (config == null) {
                opened()
                return
            }
            val value = BluetoothGattDescriptor.ENABLE_NOTIFICATION_VALUE
            if (Build.VERSION.SDK_INT >= 33) {
                gatt.writeDescriptor(config, value)
            } else {
                @Suppress("DEPRECATION")
                config.value = value
                @Suppress("DEPRECATION")
                gatt.writeDescriptor(config)
            }
        }

        override fun onDescriptorWrite(gatt: BluetoothGatt, descriptor: BluetoothGattDescriptor, status: Int) {
            opened()
        }

        private fun opened() {
            val pending = synchronized(lock) { opening.also { opening = null } }
            pending?.first?.invoke()
        }

        override fun onCharacteristicWrite(gatt: BluetoothGatt, characteristic: BluetoothGattCharacteristic, status: Int) {
            val pending = synchronized(lock) { writing.also { writing = null } } ?: return
            if (status == BluetoothGatt.GATT_SUCCESS) pending.first() else pending.second("BLE write failed (status $status)")
        }

        @Deprecated("Deprecated in Android 13")
        override fun onCharacteristicChanged(gatt: BluetoothGatt, characteristic: BluetoothGattCharacteristic) {
            if (Build.VERSION.SDK_INT < 33 && characteristic.uuid == ANSWER) {
                @Suppress("DEPRECATION")
                events.data(characteristic.value)
            }
        }

        override fun onCharacteristicChanged(gatt: BluetoothGatt, characteristic: BluetoothGattCharacteristic, value: ByteArray) {
            if (characteristic.uuid == ANSWER) events.data(value)
        }
    }

    override fun write(data: ByteArray, onDone: () -> Unit, onFail: (String) -> Unit) {
        val current = gatt
        val target = command
        if (current == null || target == null) {
            onFail("BLE link is closed")
            return
        }
        synchronized(lock) { writing = Pair(onDone, onFail) }
        // The bulb ignores write-without-response, so always ask for one.
        val started = if (Build.VERSION.SDK_INT >= 33) {
            current.writeCharacteristic(target, data, BluetoothGattCharacteristic.WRITE_TYPE_DEFAULT) == BluetoothGatt.GATT_SUCCESS
        } else {
            @Suppress("DEPRECATION")
            target.writeType = BluetoothGattCharacteristic.WRITE_TYPE_DEFAULT
            @Suppress("DEPRECATION")
            target.value = data
            @Suppress("DEPRECATION")
            current.writeCharacteristic(target)
        }
        if (!started) {
            synchronized(lock) { writing = null }
            onFail("BLE write could not start")
        }
    }

    override fun close() {
        val current = gatt
        gatt = null
        current?.disconnect()
        current?.close()
        events.closed("closed")
    }
}
