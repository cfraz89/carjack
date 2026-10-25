package io.github.cfraz89.slipstream;

import android.graphics.Bitmap;
import android.graphics.Canvas;
import android.graphics.Color;
import android.graphics.Paint;
import android.graphics.Path;

/**
 * Badges the {@link Relay} flashes over the picture when the unit's switchgear goes back or
 * selects: in an app that shows no focus, nothing else would answer the press.
 */
final class Cues {
    /** A left-pointing chevron on a dark disc. */
    static Bitmap back(int size) {
        Bitmap badge = disc(size);
        Canvas canvas = new Canvas(badge);
        Paint icon = icon(size);
        float c = size / 2f;
        float arm = size * 0.16f;
        Path chevron = new Path();
        chevron.moveTo(c + arm * 0.5f, c - arm);
        chevron.lineTo(c - arm * 0.6f, c);
        chevron.lineTo(c + arm * 0.5f, c + arm);
        canvas.drawPath(chevron, icon);
        return badge;
    }

    /** A filled dot in a ring on a dark disc. */
    static Bitmap select(int size) {
        Bitmap badge = disc(size);
        Canvas canvas = new Canvas(badge);
        Paint icon = icon(size);
        float c = size / 2f;
        canvas.drawCircle(c, c, size * 0.2f, icon);
        icon.setStyle(Paint.Style.FILL);
        canvas.drawCircle(c, c, size * 0.08f, icon);
        return badge;
    }

    private static Bitmap disc(int size) {
        Bitmap badge = Bitmap.createBitmap(size, size, Bitmap.Config.ARGB_8888);
        Paint fill = new Paint(Paint.ANTI_ALIAS_FLAG);
        fill.setColor(Color.argb(0xcc, 0x12, 0x14, 0x18));
        new Canvas(badge).drawCircle(size / 2f, size / 2f, size / 2f, fill);
        return badge;
    }

    private static Paint icon(int size) {
        Paint icon = new Paint(Paint.ANTI_ALIAS_FLAG);
        icon.setColor(Color.rgb(0xe2, 0xe2, 0xe9));
        icon.setStyle(Paint.Style.STROKE);
        icon.setStrokeWidth(size * 0.07f);
        icon.setStrokeCap(Paint.Cap.ROUND);
        icon.setStrokeJoin(Paint.Join.ROUND);
        return icon;
    }

    private Cues() {}
}
