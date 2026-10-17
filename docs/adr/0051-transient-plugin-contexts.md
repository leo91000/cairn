# Consultations éphémères des plugins

Date : 2026-10-17. Décision acceptée pendant la conception du plugin Slack.

La recherche RTS de Slack passe par une consultation éphémère réservée au
demandeur, avec son autorisation propre, sans ajout automatique des résultats aux
conversations partagées. Le modèle ordinaire des conversations Cairn conservées
et partagées ne s’applique pas à ces résultats. Le contrat RTS interdit leur
conservation et le partage d’une autorisation entre utilisateurs du service.

Une assistance par un agent n’est activée que pour les moteurs dont l’absence de
conservation est vérifiée sur le parcours complet ; sinon, seule la consultation
humaine éphémère est proposée. Masquer l’activité dans Cairn ou utiliser la sortie
CLI `secret` ne suffit pas. Les événements, sessions natives, journaux, fichiers,
exports et caches clients doivent faire partie de la preuve avant activation.

Cette voie est une capacité générique à concevoir, distincte des outils MCP et
conversations ordinaires. Elle n’autorise ni un contournement par les API de
recherche legacy ni un affaiblissement des contraintes Slack. Les synthèses et
citations produites par cette consultation restent elles aussi sans archivage,
conformément à Q18 du [cadrage](../SLACK-PLUGIN-DESIGN.md).

Référence : [Real-time Search API](https://docs.slack.dev/apis/web-api/real-time-search-api/).
