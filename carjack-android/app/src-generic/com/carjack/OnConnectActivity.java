package com.carjack;

import android.app.Activity;
import android.content.Intent;
import android.content.res.ColorStateList;
import android.graphics.Bitmap;
import android.graphics.BitmapFactory;
import android.graphics.drawable.GradientDrawable;
import android.os.Bundle;
import android.view.Gravity;
import android.view.View;
import android.view.ViewGroup;
import android.widget.FrameLayout;
import android.widget.ImageView;
import android.widget.LinearLayout;
import android.widget.TextView;

import java.io.File;

/**
 * What the unit opens on connect, picked like a wallpaper: a large thumbnail per choice, the last
 * picture that choice put on the unit, or its icon until it has been shown there.
 */
public final class OnConnectActivity extends Activity {
    private static final int REQUEST_APP = 1;
    /** The unit's picture, which the thumbnails keep the shape of. */
    private static final float ASPECT = 1056f / 800f;

    private Ui ui;
    private LinearLayout grid;

    @Override
    protected void onCreate(Bundle savedInstanceState) {
        super.onCreate(savedInstanceState);
        ui = new Ui(this);
        LinearLayout page = ui.page();
        ui.topBar(page, "On connect");
        TextView intro = ui.text("What the unit shows when CarPlay starts", 14,
                R.color.cj_on_surface_variant);
        intro.setPadding(ui.dp(16), 0, ui.dp(16), ui.dp(8));
        page.addView(intro);
        grid = new LinearLayout(this);
        grid.setOrientation(LinearLayout.VERTICAL);
        page.addView(grid);
    }

    @Override
    protected void onResume() {
        super.onResume();
        build();
    }

    private void build() {
        grid.removeAllViews();
        String current = Choices.current(this);
        LinearLayout line = null;
        String[] choices = {Choices.LAUNCHER, Previews.HOME,
                Choices.isApp(current) ? current : null};
        for (int i = 0; i < choices.length; i++) {
            if (i % 2 == 0) {
                line = new LinearLayout(this);
                grid.addView(line);
            }
            String choice = choices[i];
            View card = choice != null
                    ? card(choice, Choices.name(this, choice), detail(choice), current.equals(choice),
                            v -> {
                                if (Choices.isApp(choice)) {
                                    pickApp();
                                } else {
                                    Choices.choose(this, choice);
                                    build();
                                }
                            })
                    : card(null, "An app", "Choose one", false, v -> pickApp());
            LinearLayout.LayoutParams params = new LinearLayout.LayoutParams(0,
                    ViewGroup.LayoutParams.WRAP_CONTENT, 1);
            params.setMargins(ui.dp(6), ui.dp(6), ui.dp(6), ui.dp(6));
            line.addView(card, params);
        }
        // Keeps a lone last card half wide.
        if (choices.length % 2 == 1) {
            line.addView(new View(this), new LinearLayout.LayoutParams(0, 0, 1));
        }
    }

    private String detail(String choice) {
        return Choices.isApp(choice) ? "Tap to choose another" : Choices.detail(this, choice);
    }

    private View card(String choice, String name, String detail, boolean selected,
            View.OnClickListener onClick) {
        LinearLayout card = new LinearLayout(this);
        card.setOrientation(LinearLayout.VERTICAL);
        card.setPadding(ui.dp(6), ui.dp(6), ui.dp(6), ui.dp(10));
        card.setBackground(ui.shape(selected ? R.color.cj_primary_container
                : R.color.cj_surface_container, 24));
        card.setClipToOutline(true);
        card.setClickable(true);
        card.setForeground(new android.graphics.drawable.RippleDrawable(
                ColorStateList.valueOf(ui.color(R.color.cj_outline_variant)), null, null));
        card.setOnClickListener(onClick);

        FrameLayout thumb = new FrameLayout(this) {
            @Override
            protected void onMeasure(int widthSpec, int heightSpec) {
                int width = MeasureSpec.getSize(widthSpec);
                super.onMeasure(widthSpec, MeasureSpec.makeMeasureSpec(
                        Math.round(width * ASPECT), MeasureSpec.EXACTLY));
            }
        };
        thumb.setBackground(ui.shape(R.color.cj_surface_container_highest, 18));
        thumb.setClipToOutline(true);
        Bitmap preview = choice != null ? preview(choice) : null;
        ImageView picture = new ImageView(this);
        if (preview != null) {
            picture.setImageBitmap(preview);
            picture.setScaleType(ImageView.ScaleType.CENTER_CROP);
            thumb.addView(picture, new FrameLayout.LayoutParams(
                    ViewGroup.LayoutParams.MATCH_PARENT, ViewGroup.LayoutParams.MATCH_PARENT));
        } else {
            showIcon(picture, choice);
            thumb.addView(picture, new FrameLayout.LayoutParams(ui.dp(72), ui.dp(72),
                    Gravity.CENTER));
        }
        if (selected) {
            GradientDrawable ring = new GradientDrawable();
            ring.setCornerRadius(ui.dp(18));
            ring.setStroke(ui.dp(3), ui.color(R.color.cj_primary));
            thumb.setForeground(ring);
            ImageView check = ui.icon(R.drawable.ic_cj_check, R.color.cj_primary, 28);
            check.setBackground(ui.shape(R.color.cj_primary_container, 14));
            FrameLayout.LayoutParams at = new FrameLayout.LayoutParams(ui.dp(28), ui.dp(28),
                    Gravity.TOP | Gravity.END);
            at.setMargins(0, ui.dp(10), ui.dp(10), 0);
            thumb.addView(check, at);
        }
        card.addView(thumb);

        LinearLayout label = new LinearLayout(this);
        label.setGravity(Gravity.CENTER_VERTICAL);
        label.setPadding(ui.dp(6), ui.dp(10), ui.dp(6), 0);
        ImageView small = new ImageView(this);
        showIcon(small, choice);
        label.addView(small, new LinearLayout.LayoutParams(ui.dp(24), ui.dp(24)));
        TextView title = ui.text(name, 15, selected ? R.color.cj_on_primary_container
                : R.color.cj_on_surface);
        title.setPadding(ui.dp(8), 0, 0, 0);
        title.setSingleLine(true);
        title.setEllipsize(android.text.TextUtils.TruncateAt.END);
        label.addView(title, new LinearLayout.LayoutParams(0,
                ViewGroup.LayoutParams.WRAP_CONTENT, 1));
        card.addView(label);
        if (detail != null) {
            TextView second = ui.text(detail, 13, R.color.cj_on_surface_variant);
            second.setPadding(ui.dp(38), ui.dp(2), ui.dp(6), 0);
            second.setSingleLine(true);
            second.setEllipsize(android.text.TextUtils.TruncateAt.END);
            card.addView(second);
        }
        return card;
    }

    /** The choice's own icon; a plain glyph, in the accent, where it has none. */
    private void showIcon(ImageView view, String choice) {
        if (choice == null) {
            view.setImageResource(R.drawable.ic_cj_apps);
        } else {
            view.setImageDrawable(Choices.icon(this, choice));
        }
        if (choice == null || choice.equals(Previews.HOME) && Variant.homeApp(this) == null) {
            view.setImageTintList(ColorStateList.valueOf(ui.color(R.color.cj_primary)));
        }
    }

    private Bitmap preview(String choice) {
        File file = Previews.file(this, choice);
        return file.exists() ? BitmapFactory.decodeFile(file.getPath()) : null;
    }

    private void pickApp() {
        startActivityForResult(new Intent(this, AppPickerActivity.class), REQUEST_APP);
    }

    @Override
    protected void onActivityResult(int requestCode, int resultCode, Intent data) {
        if (requestCode == REQUEST_APP && resultCode == RESULT_OK && data != null) {
            Choices.choose(this, data.getStringExtra(AppPickerActivity.EXTRA_PACKAGE));
        }
    }
}
