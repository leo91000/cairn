# Réception HTTP publique des plugins

Date : 2026-10-17. Décision acceptée pendant la conception du plugin Slack.

Le plugin Slack doit recevoir les événements et interactions par Socket Mode et
par HTTP public, selon la configuration de l’app Slack. La proposition limitée
à Socket Mode a été écartée. Cette exigence rouvre le report des webhooks entrants
dans ADR-0045 et ajoute une capacité générique de réception HTTP à concevoir, sans
code spécifique à Slack dans Cairn, conformément à ADR-0046.

Une URL Beacon est fournie par défaut ; un propriétaire peut aussi exposer son
propre point d’entrée HTTPS direct. Beacon relaie sans conserver le contenu des
requêtes. Les secrets, la vérification et le travail restent dans l’installation,
qui doit être en ligne. Cette voie n’impose aucun port entrant aux installations
standard et ne confère pas aux requêtes externes les droits d’un membre Cairn.

La vérification avec les secrets gérés par l’hôte, les réponses rapides adaptées
aux interactions et la remise durable au plugin restent à détailler dans
[la conception](../SLACK-PLUGIN-DESIGN.md). Ce choix ne décide pas d’une
distribution publique de l’app Slack : celle-ci reste privée.
