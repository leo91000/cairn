# Archivage et suppression des conversations

Document historique : l’archivage a été retiré par [ADR-0008](adr/0008-current-disk-publication-without-archives.md), et la récupération pendant 30 jours ne concerne plus les nouvelles suppressions depuis [ADR-0041](adr/0041-immediate-conversation-deletion.md). Voir le [fonctionnement actuel](CONVERSATION-STORAGE-OPERATIONS.md).

Conception validée par l’utilisateur le 25 septembre 2026, y compris les modalités techniques et les interfaces de test. Implémentation réalisée dans le dépôt. Les résultats et limites des vérifications sont consignés dans CONVERSATION-LIFECYCLE-VALIDATION.md ; la reprise sur un véritable hôte Firecracker reste à vérifier faute de KVM dans cet environnement.

## Synthèse

| Étape | Comportement retenu |
| --- | --- |
| Actives | Vue principale par défaut ; Archives et Corbeille restent secondaires. |
| Après 30 jours d’inactivité | Archive complète vers S3, puis libération des données locales seulement après vérification. Les conversations occupées ou avec des messages à exécuter sont exclues. |
| Après 90 jours d’archivage S3 | Transition vers Glacier Flexible Retrieval. Les livrables partagés restent immédiatement accessibles. |
| Restauration explicite | Retour dans Actives avec 30 jours de grâce, ou le délai configuré. Une restauration froide peut prendre quelques heures. |
| Suppression | Mise à la corbeille pendant 30 jours ; liens publics immédiatement révoqués. Travail, attente ou envois en cours : confirmation et arrêt/annulation. |
| Récupération depuis la corbeille | Retour à l’état précédent, sans relance de travail, envoi, publication de lien ni téléchargement d’archive automatique. |
| Fin de corbeille | Effacement définitif des données associées, y compris l’archive éventuelle. |

Les archives n’expirent pas sans suppression volontaire. Les délais d’archivage sont configurables globalement dans l’application ; le bucket et les accès S3 sont configurés côté serveur.

## Décisions confirmées

- L’archivage déplace les anciennes conversations pour libérer le stockage principal.
- Une entrée reste visible dans l’application après archivage.
- Les données locales ne sont supprimées qu’après vérification de l’archive.
- L’archivage automatique intervient après 30 jours d’inactivité, avec un délai configurable.
- Un travail en cours ou une réponse attendue empêche l’archivage automatique.
- Un délai de quelques heures est acceptable pour accéder à une archive froide.
- La suppression des conversations fait partie du périmètre, avec un geste de balayage sur Android.
- La suppression place la conversation dans une corbeille pendant 30 jours, puis efface définitivement ses données associées, y compris son éventuelle archive S3/Glacier.
- Sur Android, un swipe vers la gauche, comme dans Gmail, met la conversation à la corbeille une fois le seuil franchi ; un snackbar « Annuler » la restaure aussitôt.
- L’archive conserve l’historique, les pièces jointes, les livrables, la session agent et l’environnement de travail pour permettre la reprise.
- Les archives passent à S3 Glacier Flexible Retrieval 90 jours après leur archivage S3 ; ce délai est configurable. Sans reprise, cela représente environ 120 jours depuis la dernière activité.
- Les échanges et le travail de l’agent réinitialisent l’inactivité ; une simple consultation ne la réinitialise pas.
- Les livrables partagés restent immédiatement accessibles pendant l’archivage, y compris froid. Leur accès doit donc être indépendant de la restauration de l’archive.
- Les liens publics sont révoqués dès la mise en corbeille. Restaurer la conversation ne les republie pas automatiquement.
- Supprimer une conversation avec un travail en cours ou une réponse attendue demande confirmation, puis arrête le travail ou annule l’attente avant la mise en corbeille.
- Ouvrir une archive affiche sa fiche et un bouton explicite « Restaurer ». La restauration affiche un état « Restauration en cours » ; elle n’est pas déclenchée par une simple ouverture.
- Web et Android proposent trois vues distinctes : Actives, Archives, Corbeille, recherchables par titre, agent et projet. Les conversations actives restent le point d’entrée et le centre visuel de l’interface ; Archives et Corbeille doivent rester secondaires.
- Le web propose « Supprimer » dans un menu ; la corbeille propose « Restaurer » et indique la date d’effacement prévue.
- Les archives sont conservées jusqu’à suppression volontaire, sans expiration automatique supplémentaire.
- Des réglages globaux dans l’application permettent d’activer l’archivage et de modifier ses délais. Le bucket AWS S3 et ses accès sont configurés côté serveur ; l’application ne crée pas l’infrastructure.
- L’activation inclut les conversations existantes déjà éligibles, avec aperçu de leur nombre avant activation et traitement progressif en arrière-plan.
- Si la session native restaurée est incompatible, l’historique et les fichiers restent intacts. L’application propose une nouvelle session agent utilisant ces éléments, uniquement après accord de l’utilisateur.

- Après restauration complète réussie d’une archive, la conversation revient dans Actives avec un délai de grâce égal au délai d’archivage configuré (30 jours par défaut). La simple lecture ne prolonge pas ce délai ; les échanges et le travail de l’agent relancent normalement l’inactivité.
- La récupération depuis la corbeille remet la conversation dans son état précédent (active ou archivée), sans réactiver les liens publics ni relancer l’agent. Récupérer une archive froide ne déclenche pas son téléchargement ; le bouton de restauration de l’archive reste explicite. Une conversation active récupérée bénéficie du même délai de grâce.
- Toute conversation contenant un message à exécuter est exclue de l’archivage, même si sa file est en pause. La suppression annule les envois en attente après confirmation ; leurs textes restent récupérables pendant les 30 jours de corbeille, mais leur exécution ne reprend jamais automatiquement.

## Modalités validées

- Afficher Actives par défaut. Placer l’accès aux vues Archives et Corbeille dans un menu secondaire discret, sans trois onglets de même poids visuel.
- Sérialiser les changements de cycle de vie d’une même conversation : une suppression prévaut sur un transfert ou une restauration en cours, qui ne doit jamais la faire réapparaître dans Actives.
- En cas d’échec d’archivage ou d’intégrité non vérifiée, conserver les données locales et permettre une nouvelle tentative. En cas d’échec de restauration, préserver l’archive et afficher un état permettant de réessayer.
- À expiration de la corbeille, refuser la récupération et l’accès, puis effectuer l’effacement physique de façon durable et réessayable. Un échec de stockage ne doit pas être présenté comme un effacement achevé.
- Traiter de façon idempotente les tâches de fond et les reprises après redémarrage. Les jobs de titres, notifications, messages et aperçus doivent respecter le cycle de vie de la conversation.
- Garder immédiatement disponibles les fichiers effectivement partagés par lien public ; leurs copies accessibles ne suivent pas la transition vers le stockage froid.
- Couvrir les copies gérées par l’application sur le serveur, S3 et ses clients. Les copies déjà téléchargées par un destinataire et les sauvegardes indépendantes ne sont pas révocables par ce mécanisme.

## Périmètre

Le travail couvre le backend, les fichiers et environnements de conversation, l’intégration S3/Glacier, les réglages globaux et les parcours web/Android décrits ci-dessus. Le cycle des tâches planifiées reste distinct de celui des conversations.

La création d’infrastructure AWS, un déploiement en production, la recherche plein texte dans les archives froides et une suppression définitive anticipée par l’utilisateur ne font pas partie de cette proposition. La suppression après 30 jours de corbeille est automatique. Le stockage choisi doit permettre les transitions et l’effacement prévus ; l’activation devra signaler une configuration incompatible.

## Faits relevés dans le dépôt

- L’archivage existant concerne les tâches ; aucun cycle d’archivage S3 des conversations n’a été trouvé.
- Les conversations associent des messages et événements en base, des fichiers, une session agent et un environnement de travail.
- La reprise actuelle dépend de la session agent et du workspace ; le nettoyage de celui-ci peut empêcher la reprise.
- Les livrables peuvent posséder des liens publics révocables : leur devenir doit être explicite lors d’une suppression.
- Au redémarrage, la VM utilise l’image runtime courante. Le dépôt ne fournit pas de garantie de compatibilité des sessions natives sur plusieurs mois : préserver l’archive ne suffit pas à garantir leur reprise par une version future du moteur agent.
- La reprise reste soumise aux permissions courantes de l’agent et aux contraintes de son environnement.
- La récupération au démarrage peut recréer des événements depuis les résumés des runs. L’archivage et la corbeille devront être respectés par cette récupération, ainsi que par le traitement des messages en attente, des titres et des notifications.
- Web et Android disposent de caches persistants de l’historique. La suppression devra invalider les copies gérées par les clients ; un appareil hors ligne ne peut recevoir cette invalidation qu’à sa reconnexion.

## Vérification convenue

- Exercer le cycle de vie par l’interface HTTP authentifiée existante, y compris historique et fichiers, avec passage du temps contrôlé, redémarrage et opérations concurrentes.
- Vérifier les échecs de transfert, l’intégrité avant nettoyage local, la restauration complète et l’absence de réapparition après suppression.
- Vérifier les liens publics : lecture pendant l’archivage, révocation dès mise en corbeille, absence de republication automatique après récupération.
- Exercer les parcours web avec les tests navigateur existants, y compris synchronisation entre clients et invalidation des caches.
- Exercer le swipe, la corbeille et la restauration via les tests d’interface Android sur émulateur. Les tests JVM seuls ne constituent pas une validation du geste sur appareil.
- Vérifier la reprise avec les scénarios Firecracker existants, notamment fichiers non commités et session native conservée. La compatibilité avec toutes les versions futures d’un moteur agent ne peut être promise.

## Référence pour le stockage froid

AWS distingue l’accès immédiat de Glacier Instant Retrieval et la restauration préalable de Glacier Flexible Retrieval / Deep Archive. Glacier Flexible Retrieval a été retenu avec un délai de transition de 90 jours après archivage S3.

Source : [AWS — options de restauration des archives](https://docs.aws.amazon.com/AmazonS3/latest/userguide/restoring-objects-retrieval-options.html).
