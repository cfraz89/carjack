package com.carjack;

/** Where the accessory's touches go, in its panel's pixels. */
interface Touch {
    int DOWN = 0;
    int UP = 1;
    int MOVE = 2;

    void touch(int action, float panelX, float panelY);
}
