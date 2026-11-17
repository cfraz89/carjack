// Adapted from OpenCfMoto and headunit-revived (AGPL-3.0): connection/SocketAccessoryConnection.kt,
// aap/AapSslContext.kt, ssl/SingleKeyKeyManager.kt, aap/AapReadSingleMessage.kt.
package io.github.cfraz89.slipstream.aa

import android.content.Context
import android.util.Base64
import io.github.cfraz89.slipstream.R
import java.io.DataInputStream
import java.io.IOException
import java.net.Socket
import java.net.SocketTimeoutException
import java.nio.ByteBuffer
import java.security.KeyFactory
import java.security.KeyStore
import java.security.cert.CertificateFactory
import java.security.cert.X509Certificate
import java.security.spec.PKCS8EncodedKeySpec
import javax.net.ssl.KeyManagerFactory
import javax.net.ssl.SSLContext
import javax.net.ssl.SSLEngine
import javax.net.ssl.SSLEngineResult.HandshakeStatus
import javax.net.ssl.SSLEngineResult.Status
import javax.net.ssl.X509TrustManager

/**
 * The TCP connection to Android Auto, and the TLS it runs inside its own messages: plaintext
 * version exchange, a handshake carried in ENCAPSULATED_SSL messages with the head unit as the TLS
 * client, then every message's body encrypted.
 */
class AapLink(private val socket: Socket, context: Context) {
    private companion object {
        const val READ_TIMEOUT_MS = 15_000
        const val HANDSHAKE_TIMEOUT_MS = 15_000L
        const val TYPE_ENCAPSULATED_SSL = 3
        const val TYPE_VERSION_RESPONSE = 2
        /** First, middle, last and only fragment of a message too big for one frame. */
        const val FLAG_FIRST = 0x09
        const val FLAG_ENCRYPTED = 0x08
    }

    private val input = DataInputStream(socket.getInputStream())
    private val output = socket.getOutputStream()
    private val engine: SSLEngine = sslContext(context).createSSLEngine("android-auto", 5277).apply {
        useClientMode = true
    }
    private val netBuffer = ByteBuffer.allocateDirect(engine.session.packetBufferSize)
    private val appBuffer = ByteBuffer.allocateDirect(maxOf(AapMessage.BUFFER_LENGTH, engine.session.applicationBufferSize + 50))
    private val header = ByteArray(4)
    private val body = ByteArray(4 * 1024 * 1024)
    private val skip = ByteArray(4)

    init {
        socket.tcpNoDelay = true
        socket.keepAlive = true
    }

    /** Version exchange, TLS, then authentication complete. False on any failure. */
    fun handshake(): Boolean {
        send(AapMessage.VERSION_REQUEST, AapMessage.VERSION_REQUEST.size)
        val response = readPlain() ?: return false
        if (response.first != TYPE_VERSION_RESPONSE) {
            AaLog.e("Expected a version response, got type ${response.first}")
            return false
        }
        engine.beginHandshake()
        val deadline = System.currentTimeMillis() + HANDSHAKE_TIMEOUT_MS
        var pending = ByteArray(0)
        while (engine.handshakeStatus != HandshakeStatus.FINISHED &&
            engine.handshakeStatus != HandshakeStatus.NOT_HANDSHAKING
        ) {
            if (System.currentTimeMillis() > deadline) {
                AaLog.e("TLS handshake timed out")
                return false
            }
            when (engine.handshakeStatus) {
                HandshakeStatus.NEED_WRAP -> {
                    netBuffer.clear()
                    val result = engine.wrap(ByteBuffer.allocate(0), netBuffer)
                    runTasks()
                    val out = ByteArray(result.bytesProduced())
                    netBuffer.flip()
                    netBuffer.get(out)
                    val message = AapMessage.raw(0, 3, TYPE_ENCAPSULATED_SSL, out)
                    send(message, message.size)
                }
                HandshakeStatus.NEED_UNWRAP -> {
                    if (pending.isEmpty()) pending = readPlain()?.second ?: return false
                    appBuffer.clear()
                    val data = ByteBuffer.wrap(pending)
                    val result = engine.unwrap(data, appBuffer)
                    runTasks()
                    when (result.status) {
                        Status.OK -> pending = ByteArray(data.remaining()).also { data.get(it) }
                        Status.BUFFER_UNDERFLOW -> pending += readPlain()?.second ?: return false
                        else -> {
                            AaLog.e("TLS unwrap failed: ${result.status}")
                            return false
                        }
                    }
                }
                HandshakeStatus.NEED_TASK -> runTasks()
                else -> {
                    AaLog.e("TLS handshake stuck at ${engine.handshakeStatus}")
                    return false
                }
            }
        }
        send(AapMessage.AUTH_COMPLETE, AapMessage.AUTH_COMPLETE.size)
        AaLog.i("Handshake done")
        return true
    }

    /** One plaintext message: its type and payload. */
    private fun readPlain(): Pair<Int, ByteArray>? {
        val head = ByteArray(6)
        if (!readFully(head, 6)) return null
        val length = AapMessage.getShort(head, 2) - AapMessage.TYPE_SIZE
        if (length < 0 || length > AapMessage.BUFFER_LENGTH) {
            AaLog.e("Bad plaintext length $length")
            return null
        }
        val payload = ByteArray(length)
        if (!readFully(payload, length)) return null
        return AapMessage.getShort(head, 4) to payload
    }

    private fun runTasks() {
        while (true) (engine.delegatedTask ?: return).run()
    }

    @Synchronized
    private fun send(buf: ByteArray, length: Int): Boolean = try {
        output.write(buf, 0, length)
        output.flush()
        true
    } catch (e: IOException) {
        AaLog.e("Send failed", e)
        false
    }

    /** [message] with its body encrypted. */
    fun sendEncrypted(message: AapMessage): Boolean {
        val encrypted = synchronized(engine) {
            netBuffer.clear()
            val plain = ByteBuffer.wrap(message.data, AapMessage.HEADER_SIZE, message.size - AapMessage.HEADER_SIZE)
            val result = try {
                engine.wrap(plain, netBuffer)
            } catch (e: IOException) {
                AaLog.e("Encrypt failed", e)
                return false
            }
            val out = ByteArray(AapMessage.HEADER_SIZE + result.bytesProduced())
            netBuffer.flip()
            netBuffer.get(out, AapMessage.HEADER_SIZE, result.bytesProduced())
            out
        }
        encrypted[0] = message.data[0]
        encrypted[1] = message.data[1]
        AapMessage.putShort(encrypted.size - AapMessage.HEADER_SIZE, 2, encrypted)
        return send(encrypted, encrypted.size)
    }

    /**
     * The next message, decrypted; null when the connection is gone. A message that can't be
     * read is skipped.
     */
    fun read(): AapMessage? {
        while (true) {
            if (!readFully(header, 4)) return null
            val channel = header[0].toInt()
            val flags = header[1].toInt()
            val length = AapMessage.getShort(header, 2)
            // A fragmented message's first frame carries its total size too.
            if (flags and 0xff == FLAG_FIRST && !readFully(skip, 4)) return null
            if (length > body.size) {
                AaLog.e("Message of $length bytes is too big")
                return null
            }
            if (!readFully(body, length)) return null
            if (flags and FLAG_ENCRYPTED == 0) {
                AaLog.w("Unencrypted message on channel $channel, skipped")
                continue
            }
            val plain = synchronized(engine) {
                appBuffer.clear()
                try {
                    engine.unwrap(ByteBuffer.wrap(body, 0, length), appBuffer)
                } catch (e: IOException) {
                    AaLog.e("Decrypt failed", e)
                    return null
                }
                ByteArray(appBuffer.position()).also {
                    appBuffer.flip()
                    appBuffer.get(it)
                }
            }
            if (plain.size < AapMessage.TYPE_SIZE) continue
            return AapMessage(channel, flags.toByte(), AapMessage.getShort(plain, 0), AapMessage.TYPE_SIZE, plain.size, plain)
        }
    }

    private fun readFully(buf: ByteArray, length: Int): Boolean = try {
        socket.soTimeout = READ_TIMEOUT_MS
        input.readFully(buf, 0, length)
        true
    } catch (e: SocketTimeoutException) {
        AaLog.w("Android Auto went quiet")
        false
    } catch (e: IOException) {
        false
    }

    fun close() {
        try {
            socket.close()
        } catch (ignored: IOException) {
        }
    }

    /** Presents the head unit certificate every open head unit uses; trusts the phone blindly. */
    private fun sslContext(context: Context): SSLContext {
        val cert = context.resources.openRawResource(R.raw.aa_cert).use {
            CertificateFactory.getInstance("X.509").generateCertificate(it) as X509Certificate
        }
        val pem = context.resources.openRawResource(R.raw.aa_privkey).bufferedReader().use { it.readText() }
            .replace("-----BEGIN PRIVATE KEY-----", "")
            .replace("-----END PRIVATE KEY-----", "")
            .replace("\n", "")
        val key = KeyFactory.getInstance("RSA").generatePrivate(PKCS8EncodedKeySpec(Base64.decode(pem, Base64.DEFAULT)))
        val store = KeyStore.getInstance(KeyStore.getDefaultType()).apply {
            load(null)
            setKeyEntry("headunit", key, charArrayOf(), arrayOf(cert))
        }
        val keys = KeyManagerFactory.getInstance(KeyManagerFactory.getDefaultAlgorithm()).apply {
            init(store, charArrayOf())
        }
        val trustAll = object : X509TrustManager {
            override fun checkClientTrusted(chain: Array<out X509Certificate>?, authType: String?) {}
            override fun checkServerTrusted(chain: Array<out X509Certificate>?, authType: String?) {}
            override fun getAcceptedIssuers(): Array<X509Certificate> = arrayOf()
        }
        return SSLContext.getInstance("TLS").apply { init(keys.keyManagers, arrayOf(trustAll), null) }
    }
}
