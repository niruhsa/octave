ALTER TABLE equalizer_device_rules
    ADD COLUMN bass_boost_percent INTEGER NOT NULL DEFAULT 0
        CHECK (bass_boost_percent BETWEEN 0 AND 100),
    ADD COLUMN treble_boost_percent INTEGER NOT NULL DEFAULT 0
        CHECK (treble_boost_percent BETWEEN 0 AND 100);
