# Entry point started by app_process.
-keep class dev.sentinel.agent.Main { public static void main(java.lang.String[]); }
# Readable stack traces in logs.
-dontobfuscate
-keepattributes SourceFile,LineNumberTable
# Compile-time-only annotations referenced by Kotlin code.
-dontwarn org.jetbrains.annotations.**
