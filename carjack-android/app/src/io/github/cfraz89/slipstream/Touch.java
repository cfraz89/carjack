package io.github.cfraz89.slipstream;

/** Where the accessory's touches go, in its panel's pixels, a pointer id per finger. */
interface Touch {
    int DOWN = 0;
    int UP = 1;
    int MOVE = 2;

    void touch(int action, int pointer, float panelX, float panelY);
}
