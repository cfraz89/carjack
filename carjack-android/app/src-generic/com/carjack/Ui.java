package com.carjack;

import android.app.Activity;
import android.content.res.ColorStateList;
import android.graphics.Insets;
import android.graphics.Typeface;
import android.graphics.drawable.Drawable;
import android.graphics.drawable.GradientDrawable;
import android.graphics.drawable.RippleDrawable;
import android.view.Gravity;
import android.view.View;
import android.view.ViewGroup;
import android.view.WindowInsets;
import android.widget.FrameLayout;
import android.widget.ImageView;
import android.widget.LinearLayout;
import android.widget.ScrollView;
import android.widget.TextView;

/** The app's few building blocks, in Material 3's shapes and carjack's palette. */
final class Ui {
    private final Activity activity;

    Ui(Activity activity) {
        this.activity = activity;
    }

    int dp(float value) {
        return Math.round(value * activity.getResources().getDisplayMetrics().density);
    }

    int color(int id) {
        return activity.getColor(id);
    }

    /** A scrolling page drawn edge to edge, kept clear of the system bars. */
    LinearLayout page() {
        LinearLayout page = new LinearLayout(activity);
        page.setOrientation(LinearLayout.VERTICAL);
        page.setPadding(dp(16), dp(8), dp(16), dp(24));
        ScrollView scroll = new ScrollView(activity);
        scroll.setBackgroundColor(color(R.color.cj_surface));
        scroll.setClipToPadding(false);
        scroll.addView(page);
        scroll.setOnApplyWindowInsetsListener((v, insets) -> {
            Insets bars = insets.getInsets(WindowInsets.Type.systemBars());
            v.setPadding(bars.left, bars.top, bars.right, bars.bottom);
            return WindowInsets.CONSUMED;
        });
        activity.setContentView(scroll);
        return page;
    }

    /** M3's small top app bar: back, then the title. */
    void topBar(LinearLayout page, String title) {
        LinearLayout bar = new LinearLayout(activity);
        bar.setGravity(Gravity.CENTER_VERTICAL);
        bar.setMinimumHeight(dp(64));
        ImageView back = icon(R.drawable.ic_cj_arrow_back, R.color.cj_on_surface, 24);
        back.setPadding(dp(12), dp(12), dp(12), dp(12));
        back.setBackground(new RippleDrawable(
                ColorStateList.valueOf(color(R.color.cj_outline_variant)), null, null));
        back.setContentDescription("Back");
        back.setOnClickListener(v -> activity.finish());
        bar.addView(back, new LinearLayout.LayoutParams(dp(48), dp(48)));
        TextView name = text(title, 22, R.color.cj_on_surface);
        name.setPadding(dp(8), 0, 0, 0);
        bar.addView(name);
        page.addView(bar);
    }

    TextView text(String value, float sp, int colorId) {
        TextView view = new TextView(activity);
        view.setText(value);
        view.setTextSize(sp);
        view.setTextColor(color(colorId));
        return view;
    }

    TextView heading(String value) {
        TextView view = text(value, 14, R.color.cj_primary);
        view.setTypeface(Typeface.create("sans-serif-medium", Typeface.NORMAL));
        view.setPadding(dp(16), dp(20), dp(16), dp(8));
        return view;
    }

    ImageView icon(int drawable, int tintId, int sizeDp) {
        ImageView view = new ImageView(activity);
        view.setImageResource(drawable);
        view.setImageTintList(ColorStateList.valueOf(color(tintId)));
        view.setLayoutParams(new LinearLayout.LayoutParams(dp(sizeDp), dp(sizeDp)));
        return view;
    }

    GradientDrawable shape(int colorId, float radiusDp) {
        GradientDrawable shape = new GradientDrawable();
        shape.setColor(color(colorId));
        shape.setCornerRadius(dp(radiusDp));
        return shape;
    }

    /** A filled card; {@code onClick} makes it ripple. */
    LinearLayout card(LinearLayout parent, int colorId) {
        LinearLayout card = new LinearLayout(activity);
        card.setOrientation(LinearLayout.VERTICAL);
        card.setBackground(shape(colorId, 28));
        card.setClipToOutline(true);
        LinearLayout.LayoutParams params = new LinearLayout.LayoutParams(
                ViewGroup.LayoutParams.MATCH_PARENT, ViewGroup.LayoutParams.WRAP_CONTENT);
        params.topMargin = dp(12);
        parent.addView(card, params);
        return card;
    }

    void ripple(View view) {
        view.setBackground(new RippleDrawable(
                ColorStateList.valueOf(color(R.color.cj_outline_variant)), null,
                shape(R.color.cj_surface, 0)));
        view.setClickable(true);
    }

    /**
     * A list row: a leading icon in a tonal circle, a title over a supporting line, and an
     * optional trailing view. Returns the supporting line, for updating.
     */
    TextView row(LinearLayout parent, Drawable leading, boolean tintLeading, String title,
            String supporting, View trailing, View.OnClickListener onClick) {
        LinearLayout row = new LinearLayout(activity);
        row.setGravity(Gravity.CENTER_VERTICAL);
        row.setPadding(dp(16), dp(12), dp(16), dp(12));
        row.setMinimumHeight(dp(72));

        FrameLayout badge = new FrameLayout(activity);
        ImageView image = new ImageView(activity);
        image.setImageDrawable(leading);
        if (tintLeading) {
            badge.setBackground(shape(R.color.cj_primary_container, 20));
            image.setImageTintList(ColorStateList.valueOf(color(R.color.cj_on_primary_container)));
            badge.addView(image, new FrameLayout.LayoutParams(dp(24), dp(24), Gravity.CENTER));
        } else {
            badge.addView(image, new FrameLayout.LayoutParams(dp(40), dp(40), Gravity.CENTER));
        }
        row.addView(badge, new LinearLayout.LayoutParams(dp(40), dp(40)));

        LinearLayout words = new LinearLayout(activity);
        words.setOrientation(LinearLayout.VERTICAL);
        words.setPadding(dp(16), 0, dp(8), 0);
        words.addView(text(title, 16, R.color.cj_on_surface));
        TextView second = text(supporting, 14, R.color.cj_on_surface_variant);
        second.setVisibility(supporting == null ? View.GONE : View.VISIBLE);
        words.addView(second);
        row.addView(words, new LinearLayout.LayoutParams(0,
                ViewGroup.LayoutParams.WRAP_CONTENT, 1));
        if (trailing != null) {
            row.addView(trailing);
        }
        if (onClick != null) {
            ripple(row);
            row.setOnClickListener(onClick);
        }
        parent.addView(row);
        return second;
    }

    ImageView chevron() {
        return icon(R.drawable.ic_cj_chevron, R.color.cj_on_surface_variant, 24);
    }

    /** M3 buttons: filled, or text only. */
    TextView button(String label, boolean filled, View.OnClickListener onClick) {
        TextView button = text(label, 14, filled ? R.color.cj_on_primary : R.color.cj_primary);
        button.setTypeface(Typeface.create("sans-serif-medium", Typeface.NORMAL));
        button.setGravity(Gravity.CENTER);
        button.setMinHeight(dp(40));
        button.setPadding(dp(filled ? 24 : 12), 0, dp(filled ? 24 : 12), 0);
        button.setBackground(new RippleDrawable(
                ColorStateList.valueOf(color(R.color.cj_outline_variant)),
                filled ? shape(R.color.cj_primary, 20) : null, shape(R.color.cj_primary, 20)));
        button.setClickable(true);
        button.setOnClickListener(onClick);
        return button;
    }
}
