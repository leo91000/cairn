package dev.leo.manager.data

/** The same account and installation envelope that the official service returns. */
fun officialAccountFixture(csrf: String = "fixture"): String =
    """{"authenticated":true,"csrf":"$csrf","account":{"id":"fixture-account","email":"owner@example.test"},"installations":[{"id":"fixture","name":"Installation de test","role":"owner","online":true}]}"""

fun officialInstallationsFixture(): String =
    """[{"id":"fixture","name":"Installation de test","role":"owner","online":true}]"""
