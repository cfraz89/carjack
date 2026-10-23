package io.github.cfraz89.slipstream;

import android.content.Context;
import android.graphics.Bitmap;
import android.graphics.Canvas;
import android.graphics.Color;
import android.graphics.Paint;
import android.graphics.Path;

/**
 * A back and home bar along the bottom of the unit's picture, for where its switchgear can't do
 * either. Drawn into the stream by the {@link Relay} below the display, rather than as a window on
 * it, so it sits over any app; touches on it are taken before they reach the display.
 */
final class NavBar {
    static final int KEYCODE_BACK = 4;
    /** Of the view's height: about 48dp on a 7" unit. */
    private static final float HEIGHT_FRACTION = 1 / 14f;
    private static final int MIN_HEIGHT = 40;

    /** The bar's height in panel pixels; 0 where it's off. */
    static int height(Context context, int viewHeight) {
        if (!Boolean.parseBoolean(Variant.setting(context, "nav_bar", "false"))) {
            return 0;
        }
        return Math.max(MIN_HEIGHT, Math.round(viewHeight * HEIGHT_FRACTION));
    }

    /** The bar as drawn: dark, a back arrow left of centre and a home ring right of it. */
    static Bitmap render(int width, int height) {
        Bitmap bar = Bitmap.createBitmap(width, height, Bitmap.Config.ARGB_8888);
        Canvas canvas = new Canvas(bar);
        canvas.drawColor(Color.rgb(0x12, 0x14, 0x18));
        Paint icon = new Paint(Paint.ANTI_ALIAS_FLAG);
        icon.setColor(Color.rgb(0xe2, 0xe2, 0xe9));
        float size = height * 0.36f;
        float y = height / 2f;

        float backX = width / 3f;
        Path back = new Path();
        back.moveTo(backX - size * 0.6f, y);
        back.lineTo(backX + size * 0.5f, y - size * 0.6f);
        back.lineTo(backX + size * 0.5f, y + size * 0.6f);
        back.close();
        canvas.drawPath(back, icon);

        icon.setStyle(Paint.Style.STROKE);
        icon.setStrokeWidth(height * 0.07f);
        canvas.drawCircle(width * 2 / 3f, y, size * 0.6f, icon);
        return bar;
    }

    /**
     * {@code display} for touches above the bar; on it, the left half is back and the right half
     * home, done when the finger lifts where it went down.
     */
    static Touch touch(Touch display, int barX, int barY, int barWidth, int barHeight,
            Runnable back, Runnable home) {
        return new Touch() {
            /** Per pointer: which button it went down on (1 back, 2 home), or 0 for the display. */
            private final int[] pressed = new int[16];

            @Override
            public void touch(int action, int pointer, float panelX, float panelY) {
                int slot = Math.floorMod(pointer, pressed.length);
                boolean onBar = panelY >= barY && panelY < barY + barHeight
                        && panelX >= barX && panelX < barX + barWidth;
                int button = !onBar ? 0 : panelX < barX + barWidth / 2f ? 1 : 2;
                if (action == Touch.DOWN) {
                    pressed[slot] = button;
                }
                if (pressed[slot] == 0) {
                    display.touch(action, pointer, panelX, panelY);
                    return;
                }
                if (action == Touch.UP) {
                    if (button == pressed[slot]) {
                        (button == 1 ? back : home).run();
                    }
                    pressed[slot] = 0;
                }
            }
        };
    }

    private NavBar() {}
}
