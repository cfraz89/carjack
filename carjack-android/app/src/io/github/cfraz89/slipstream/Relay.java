package io.github.cfraz89.slipstream;

import android.graphics.SurfaceTexture;
import android.opengl.EGL14;
import android.opengl.EGLConfig;
import android.opengl.EGLContext;
import android.opengl.EGLDisplay;
import android.opengl.EGLExt;
import android.opengl.EGLSurface;
import android.opengl.GLES11Ext;
import android.opengl.GLES20;
import android.os.Handler;
import android.os.HandlerThread;
import android.os.SystemClock;
import android.view.Surface;

import java.nio.ByteBuffer;
import java.nio.ByteOrder;
import java.nio.FloatBuffer;
import java.util.concurrent.CountDownLatch;

/**
 * Draws the display's frames into the encoder's input: into part of it, for an accessory that keeps
 * a strip of its panel for itself (the stream covers the whole panel, the picture only the view),
 * and again on a still screen, which would otherwise leave the encoder with nothing to send.
 */
final class Relay {
    private static final String VERTEX = ""
            + "attribute vec2 pos;\n"
            + "uniform mat4 tex;\n"
            + "varying vec2 uv;\n"
            + "void main() {\n"
            + "  uv = (tex * vec4(pos * 0.5 + 0.5, 0.0, 1.0)).xy;\n"
            + "  gl_Position = vec4(pos, 0.0, 1.0);\n"
            + "}\n";
    private static final String FRAGMENT = ""
            + "#extension GL_OES_EGL_image_external : require\n"
            + "precision mediump float;\n"
            + "uniform samplerExternalOES frame;\n"
            + "varying vec2 uv;\n"
            + "void main() { gl_FragColor = texture2D(frame, uv); }\n";
    private static final float[] QUAD = {-1, -1, 1, -1, -1, 1, 1, 1};

    private final Surface output;
    private final int width;
    private final int height;
    private final int viewX;
    private final int viewY;
    private final int viewWidth;
    private final int viewHeight;
    private final HandlerThread thread = new HandlerThread("carjack-relay");
    private Handler handler;
    private EGLDisplay eglDisplay;
    private EGLContext eglContext;
    private EGLSurface eglSurface;
    private SurfaceTexture texture;
    private Surface input;
    private int program;
    /** GL reads the vertices from here at every draw, so it must outlive setUp. */
    private FloatBuffer quad;
    private final float[] matrix = new float[16];
    private static final long IDLE_REDRAW_MS = 500;
    private boolean haveFrame;
    private long lastDrawMs;
    private long lastTimestampNs;
    private static final int CAPTURE_WIDTH = 480;
    private java.util.function.Consumer<android.graphics.Bitmap> pendingCapture;

    Relay(Surface output, int width, int height, int viewX, int viewY, int viewWidth,
            int viewHeight) {
        this.output = output;
        this.width = width;
        this.height = height;
        this.viewX = viewX;
        this.viewY = viewY;
        this.viewWidth = viewWidth;
        this.viewHeight = viewHeight;
    }

    /** Returns the surface the display should render into. */
    Surface start() {
        thread.start();
        handler = new Handler(thread.getLooper());
        CountDownLatch ready = new CountDownLatch(1);
        handler.post(() -> {
            setUp();
            ready.countDown();
        });
        handler.postDelayed(idleRedraw, IDLE_REDRAW_MS);
        try {
            ready.await();
        } catch (InterruptedException e) {
            Thread.currentThread().interrupt();
        }
        return input;
    }

    private void setUp() {
        eglDisplay = EGL14.eglGetDisplay(EGL14.EGL_DEFAULT_DISPLAY);
        int[] version = new int[2];
        EGL14.eglInitialize(eglDisplay, version, 0, version, 1);
        int[] attribs = {
            EGL14.EGL_RED_SIZE, 8, EGL14.EGL_GREEN_SIZE, 8, EGL14.EGL_BLUE_SIZE, 8,
            EGL14.EGL_RENDERABLE_TYPE, EGL14.EGL_OPENGL_ES2_BIT,
            EGLExt.EGL_RECORDABLE_ANDROID, 1, EGL14.EGL_NONE,
        };
        EGLConfig[] configs = new EGLConfig[1];
        int[] count = new int[1];
        EGL14.eglChooseConfig(eglDisplay, attribs, 0, configs, 0, 1, count, 0);
        eglContext = EGL14.eglCreateContext(eglDisplay, configs[0], EGL14.EGL_NO_CONTEXT,
                new int[] {EGL14.EGL_CONTEXT_CLIENT_VERSION, 2, EGL14.EGL_NONE}, 0);
        eglSurface = EGL14.eglCreateWindowSurface(eglDisplay, configs[0], output,
                new int[] {EGL14.EGL_NONE}, 0);
        EGL14.eglMakeCurrent(eglDisplay, eglSurface, eglSurface, eglContext);

        program = link(VERTEX, FRAGMENT);
        int[] tex = new int[1];
        GLES20.glGenTextures(1, tex, 0);
        GLES20.glBindTexture(GLES11Ext.GL_TEXTURE_EXTERNAL_OES, tex[0]);
        GLES20.glTexParameteri(GLES11Ext.GL_TEXTURE_EXTERNAL_OES, GLES20.GL_TEXTURE_MIN_FILTER,
                GLES20.GL_LINEAR);
        GLES20.glTexParameteri(GLES11Ext.GL_TEXTURE_EXTERNAL_OES, GLES20.GL_TEXTURE_MAG_FILTER,
                GLES20.GL_LINEAR);
        texture = new SurfaceTexture(tex[0]);
        texture.setDefaultBufferSize(viewWidth, viewHeight);
        texture.setOnFrameAvailableListener(t -> draw(), handler);
        input = new Surface(texture);

        quad = ByteBuffer.allocateDirect(QUAD.length * 4)
                .order(ByteOrder.nativeOrder()).asFloatBuffer().put(QUAD);
        quad.position(0);
        int pos = GLES20.glGetAttribLocation(program, "pos");
        GLES20.glEnableVertexAttribArray(pos);
        GLES20.glVertexAttribPointer(pos, 2, GLES20.GL_FLOAT, false, 0, quad);
        GLES20.glUseProgram(program);
    }

    private void draw() {
        if (texture == null) {
            return;
        }
        texture.updateTexImage();
        texture.getTransformMatrix(matrix);
        haveFrame = true;
        render(texture.getTimestamp());
    }

    /**
     * A still screen stops producing frames, and the encoder repeats the last one only a few
     * times; a unit left without frames shows black. So the last frame is drawn again when the
     * display has been idle a while.
     */
    private final Runnable idleRedraw = new Runnable() {
        @Override
        public void run() {
            if (texture == null) {
                return;
            }
            if (haveFrame && SystemClock.uptimeMillis() - lastDrawMs >= IDLE_REDRAW_MS) {
                render(System.nanoTime());
            }
            handler.postDelayed(this, IDLE_REDRAW_MS);
        }
    };

    /** The picture as the unit next gets it, {@link #CAPTURE_WIDTH} wide, to {@code sink}. */
    void capture(java.util.function.Consumer<android.graphics.Bitmap> sink) {
        handler.post(() -> {
            if (texture == null || !haveFrame) {
                return;
            }
            pendingCapture = sink;
            render(System.nanoTime());
        });
    }

    /** The view, the right way up: GL's rows run bottom to top. */
    private android.graphics.Bitmap readView() {
        ByteBuffer pixels = ByteBuffer.allocateDirect(viewWidth * viewHeight * 4);
        GLES20.glReadPixels(viewX, height - viewY - viewHeight, viewWidth, viewHeight,
                GLES20.GL_RGBA, GLES20.GL_UNSIGNED_BYTE, pixels);
        android.graphics.Bitmap raw = android.graphics.Bitmap.createBitmap(viewWidth, viewHeight,
                android.graphics.Bitmap.Config.ARGB_8888);
        raw.copyPixelsFromBuffer(pixels);
        float scale = (float) CAPTURE_WIDTH / viewWidth;
        android.graphics.Matrix flip = new android.graphics.Matrix();
        flip.setScale(scale, -scale);
        android.graphics.Bitmap upright = android.graphics.Bitmap.createBitmap(raw, 0, 0,
                viewWidth, viewHeight, flip, true);
        raw.recycle();
        return upright;
    }

    private void render(long timestampNs) {
        // Strictly increasing, whether the time came from the display or the clock.
        timestampNs = Math.max(timestampNs, lastTimestampNs + 1);
        lastTimestampNs = timestampNs;
        lastDrawMs = SystemClock.uptimeMillis();
        GLES20.glViewport(0, 0, width, height);
        GLES20.glClearColor(0, 0, 0, 1);
        GLES20.glClear(GLES20.GL_COLOR_BUFFER_BIT);
        // GL's origin is bottom left; the view's is top left.
        GLES20.glViewport(viewX, height - viewY - viewHeight, viewWidth, viewHeight);
        GLES20.glUniformMatrix4fv(GLES20.glGetUniformLocation(program, "tex"), 1, false, matrix,
                0);
        GLES20.glDrawArrays(GLES20.GL_TRIANGLE_STRIP, 0, 4);
        if (pendingCapture != null) {
            pendingCapture.accept(readView());
            pendingCapture = null;
        }
        EGLExt.eglPresentationTimeANDROID(eglDisplay, eglSurface, timestampNs);
        EGL14.eglSwapBuffers(eglDisplay, eglSurface);
    }

    private static int link(String vertex, String fragment) {
        int program = GLES20.glCreateProgram();
        GLES20.glAttachShader(program, compile(GLES20.GL_VERTEX_SHADER, vertex));
        GLES20.glAttachShader(program, compile(GLES20.GL_FRAGMENT_SHADER, fragment));
        GLES20.glLinkProgram(program);
        return program;
    }

    private static int compile(int type, String source) {
        int shader = GLES20.glCreateShader(type);
        GLES20.glShaderSource(shader, source);
        GLES20.glCompileShader(shader);
        return shader;
    }

    void stop() {
        CountDownLatch done = new CountDownLatch(1);
        handler.post(() -> {
            if (texture != null) {
                texture.release();
                texture = null;
            }
            if (input != null) {
                input.release();
            }
            EGL14.eglMakeCurrent(eglDisplay, EGL14.EGL_NO_SURFACE, EGL14.EGL_NO_SURFACE,
                    EGL14.EGL_NO_CONTEXT);
            EGL14.eglDestroySurface(eglDisplay, eglSurface);
            EGL14.eglDestroyContext(eglDisplay, eglContext);
            EGL14.eglTerminate(eglDisplay);
            done.countDown();
        });
        try {
            done.await();
        } catch (InterruptedException e) {
            Thread.currentThread().interrupt();
        }
        thread.quitSafely();
    }
}
