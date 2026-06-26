import java.util.Properties

plugins {
    id("com.android.application")
    id("org.jetbrains.kotlin.android")
    id("rust")
}

val tauriProperties = Properties().apply {
    val propFile = file("tauri.properties")
    if (propFile.exists()) {
        propFile.inputStream().use { load(it) }
    }
}

android {
    compileSdk = 36
    namespace = "dev.niruhsa.octave"
    defaultConfig {
        manifestPlaceholders["usesCleartextTraffic"] = "false"
        applicationId = "dev.niruhsa.octave"
        minSdk = 24
        targetSdk = 36
        versionCode = tauriProperties.getProperty("tauri.android.versionCode", "1").toInt()
        versionName = tauriProperties.getProperty("tauri.android.versionName", "1.0")
    }
    buildTypes {
        getByName("debug") {
            manifestPlaceholders["usesCleartextTraffic"] = "true"
            isDebuggable = true
            isJniDebuggable = true
            isMinifyEnabled = false
            packaging {                jniLibs.keepDebugSymbols.add("*/arm64-v8a/*.so")
                jniLibs.keepDebugSymbols.add("*/armeabi-v7a/*.so")
                jniLibs.keepDebugSymbols.add("*/x86/*.so")
                jniLibs.keepDebugSymbols.add("*/x86_64/*.so")
            }
        }
        getByName("release") {
            isMinifyEnabled = true
            proguardFiles(
                *fileTree(".") { include("**/*.pro") }
                    .plus(getDefaultProguardFile("proguard-android-optimize.txt"))
                    .toList().toTypedArray()
            )
        }
    }
    kotlinOptions {
        jvmTarget = "1.8"
    }
    buildFeatures {
        buildConfig = true
    }
}

rust {
    rootDirRel = "../../../"
}

dependencies {
    implementation("androidx.webkit:webkit:1.14.0")
    implementation("androidx.appcompat:appcompat:1.7.1")
    implementation("androidx.activity:activity-ktx:1.10.1")
    implementation("com.google.android.material:material:1.12.0")
    implementation("androidx.lifecycle:lifecycle-process:2.10.0")
    // MediaSessionCompat + MediaStyle notification + MediaButtonReceiver for the
    // native media notification (see MediaSessionPlugin / MediaService).
    implementation("androidx.media:media:1.7.0")
    // WorkManager: periodic background job that polls the server's notification
    // feed while the app is closed and posts new-release notifications (see
    // NotificationPollWorker / NotificationSyncPlugin). Pulls in coroutines for
    // CoroutineWorker.
    implementation("androidx.work:work-runtime-ktx:2.9.1")
    // Firebase Cloud Messaging (Phase 10): real-time push notifications. The
    // dependency is always present, but the google-services plugin (which wires
    // FirebaseApp from google-services.json) is only applied below when that
    // file exists — so without Firebase configured, FCM init fails gracefully
    // and the app falls back to the WorkManager poll.
    implementation(platform("com.google.firebase:firebase-bom:33.7.0"))
    implementation("com.google.firebase:firebase-messaging")
    testImplementation("junit:junit:4.13.2")
    androidTestImplementation("androidx.test.ext:junit:1.1.4")
    androidTestImplementation("androidx.test.espresso:espresso-core:3.5.0")
}

apply(from = "tauri.build.gradle.kts")

// Apply the Firebase google-services plugin only when the config file is
// present, so the project builds without Firebase set up (FCM then no-ops and
// the app falls back to the WorkManager poll). Drop the Firebase console's
// `google-services.json` into this directory (app/) to enable real-time push.
if (file("google-services.json").exists()) {
    apply(plugin = "com.google.gms.google-services")
}