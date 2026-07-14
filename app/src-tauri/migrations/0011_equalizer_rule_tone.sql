ALTER TABLE equalizer_synced_device_rules
    ADD COLUMN bass_boost_percent INTEGER NOT NULL DEFAULT 0
        CHECK (bass_boost_percent BETWEEN 0 AND 100);

ALTER TABLE equalizer_synced_device_rules
    ADD COLUMN treble_boost_percent INTEGER NOT NULL DEFAULT 0
        CHECK (treble_boost_percent BETWEEN 0 AND 100);

ALTER TABLE equalizer_local_device_rules
    ADD COLUMN bass_boost_percent INTEGER NOT NULL DEFAULT 0
        CHECK (bass_boost_percent BETWEEN 0 AND 100);

ALTER TABLE equalizer_local_device_rules
    ADD COLUMN treble_boost_percent INTEGER NOT NULL DEFAULT 0
        CHECK (treble_boost_percent BETWEEN 0 AND 100);
