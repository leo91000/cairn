# Enrichissement des questions d’un agent

Cadrage et synthèse validés par le propriétaire le 27 octobre 2026. Ce document
décrit le comportement attendu de l’implémentation, indépendamment de son
déploiement.

## Périmètre confirmé

Les questions des conversations Cairn permettent de sélectionner plusieurs choix
et d’y ajouter un commentaire de réponse. Ces possibilités sont communes à Codex,
Claude Code et OpenCode, avec une présentation native sur le web et Android.
Les opérations correspondantes conservent la parité avec le MCP public et
`cairn_installation`, ainsi que leurs contrôles d’autorisation.

## Décisions confirmées

- L’agent choisit le mode unique ou multiple pour chaque question. Le choix unique
  reste le mode par défaut.
- En mode multiple, l’agent peut préciser un minimum ou un maximum de choix.
- Un commentaire de réponse facultatif accompagne un choix unique ou plusieurs
  choix. Écrire ce commentaire ne désélectionne pas les choix retenus.
- Une réponse uniquement en texte libre reste possible lorsque les propositions
  ne conviennent pas.
- Une recommandation est signalée visuellement, sans présélection. La personne
  effectue elle-même sa sélection et envoie explicitement sa réponse.
- Les bornes concernent uniquement les choix cochés. Un commentaire ne compte pas
  comme un choix et ne compense pas une sélection insuffisante ; une réponse
  uniquement en texte libre reste acceptée.
- Un groupe est envoyé en une fois, avec une réponse à chaque question. Une
  question vide empêche l’envoi. Retirer une demande obsolète reste distinct de
  répondre.
- La réponse transmise à l’agent distingue les choix sélectionnés du commentaire.
- Un outil géré par Cairn porte le contrat commun aux trois agents de code. La
  compatibilité des anciens appels à cet outil n’est pas une exigence : le
  propriétaire préfère un contrat solide à une couche de transition, puisqu’il
  n’a pas de clients à préserver.
- L’outil crée le questionnaire et retourne immédiatement son identifiant.
  L’agent poursuit un travail indépendant ou termine son tour avec `needs_input`
  lorsqu’il dépend de la réponse. Le questionnaire reste disponible entre les
  tours ; l’envoi des réponses reprend la conversation ou rejoint son exécution
  en cours.
- L’historique des anciens échanges reste lisible. Les anciens questionnaires
  encore en attente sont retirés au passage au nouveau format, sans conversion ;
  l’agent peut les reposer si nécessaire.
- Une réponse envoyée est figée. Un changement d’avis se formule dans un nouveau
  message, sans modifier rétroactivement la réponse reçue par l’agent.
- Le nouvel outil Cairn est le seul chemin de questionnement. Les outils natifs
  sont désactivés lorsque le moteur le permet. Un appel natif résiduel est refusé
  avec une indication d’utiliser l’outil Cairn, sans maintien d’un second contrat.
- Une simple fin de tour conserve les questionnaires en attente, tout comme un
  résultat `needs_input`. Lorsque l’agent déclare la tâche terminée (`completed`),
  les questionnaires encore en attente sont retirés automatiquement. Un retrait
  manuel reste possible et ne constitue jamais une réponse ni une autorisation.

Exemple : sélectionner « Android » et « Web », puis ajouter « Android en
priorité ».

Le vocabulaire de référence figure dans [CONTEXT.md](../CONTEXT.md). Le choix d’un
outil commun est enregistré dans
[l’ADR-0074](adr/0074-common-agent-question-tool.md).

## Validation du cadrage

Les arbitrages fonctionnels et la synthèse d’ensemble sont confirmés.

## Exemples d’acceptation

| Situation | Résultat attendu |
| --- | --- |
| Question sans mode explicite | Choix unique, sans présélection |
| Choisir « Android » et « Web », puis écrire « Android en priorité » | Les deux choix sont conservés et le commentaire est transmis séparément |
| Question à choix unique avec commentaire | Un seul choix sélectionné, plus le commentaire facultatif |
| Deux choix minimum, un choix coché et un commentaire | Envoi refusé tant que la sélection reste insuffisante |
| Deux choix minimum, aucun choix et une réponse en texte libre | Réponse acceptée |
| Au moins une question du groupe entièrement vide | Envoi du groupe refusé |
| Une option recommandée | Indication visible, aucune sélection automatique |
| Création d’un questionnaire | Identifiant retourné immédiatement ; réponse possible plus tard |
| Fin de tour ou changement d’agent de code pendant l’attente | Questionnaire Cairn toujours disponible |
| Résultat `completed` avec des questionnaires en attente | Retrait des questionnaires sans réponse ni accord implicite |
| Répétition de la même requête de création ou d’envoi | Pas de questionnaire ni de réponse en double |
| Correction après envoi | Nouveau message ; la réponse déjà envoyée reste figée |

## Contraintes de livraison

- Distinguer les questionnaires Cairn des requêtes natives : la fermeture d’une
  requête native, une fin de tour ou un changement d’agent de code ne doit pas
  retirer un questionnaire Cairn qui attend toujours une réponse.
- Livrer les choix et le commentaire aux trois agents de code, y compris après
  une reprise de conversation, sans dépendre d’un identifiant de requête native.
- Vérifier la validation côté serveur des modes, des bornes et des réponses,
  ainsi que l’absence de double création ou de double livraison lors d’une
  répétition de requête.
- Conserver les contrôles d’autorisation, le retrait sans réponse implicite et
  les règles de confidentialité existantes.
- Vérifier les mêmes cas de sélection et de commentaire sur web, Android, MCP
  public et `cairn_installation`.
- Exposer aussi le retrait via le MCP public et `cairn_installation` pour respecter
  la parité de cette capacité avec les interfaces.

## Contrat livré

`cairn_workspace.ask_questions` reçoit un `requestId` UUID et une liste de
`questions`. Chaque champ possède un identifiant, un titre, un mode `single` ou
`multiple`, des bornes facultatives et des options identifiées. Une option peut
porter une description et une recommandation explicite. Répéter le même
`requestId` avec le même contenu retourne le même questionnaire ; le réutiliser
avec un contenu différent est refusé.

Le questionnaire contient au plus 20 champs, chacun avec au plus 30 options, et
sa représentation JSON normalisée ne dépasse pas 64 Kio. Chaque commentaire est
limité à 10 000 caractères. La réponse conserve, pour chaque champ, `selected`
(identifiants des options) et `comment` séparément. Les libellés des options
sélectionnées accompagnent aussi le message transmis à l’agent.

Les réponses sont validées côté serveur, indépendamment de l’interface utilisée.
Les réponses longues respectant ce contrat ne passent pas par la limite de
50 000 caractères des messages ordinaires. Leur livraison utilise les mêmes
accusés de réception et les mêmes protections de confidentialité que la
conversation.

## Vérification de l’implémentation

Validation locale le 28 octobre 2026 :

- `pnpm check` : 83 fichiers de tests, 655 tests, vérification TypeScript et build.
- Suite backend du workspace exécutée avec le lanceur isolant les identifiants
  réels. Après adaptation des tests de l’ancien contrat, la bibliothèque
  installation passe ses 635 tests et la suite worker ses 40 tests. Le seul
  écart de permissions de fichiers a été reproduit avec le masque restrictif du
  processus, puis vérifié avec le masque standard `0022`, sans modification du
  comportement de transfert.
- Clippy du workspace avec tous les targets et avertissements traités en erreurs.
- Tests navigateur des questionnaires sous Chromium et WebKit, ainsi que les
  parcours des trois agents. Les suites Claude Code et OpenCode passent leurs
  18 parcours.
- Android : formatage, tests unitaires et lint ; les deux scénarios de retrait
  et de choix multiples avec commentaire passent aussi sur émulateur Android 16.
- Revue du contrat et revue des standards : aucune remarque ouverte.

Ces résultats concernent le code local et les simulateurs de moteurs utilisés
par les tests ; ils ne constituent pas une validation de déploiement.

## Référence avant cette évolution

L’ancien contrat limitait chaque réponse à une seule chaîne non vide. Le web et
Android présentaient des choix uniques et du texte libre mutuellement exclusifs.
Le serveur exigeait déjà une réponse pour chaque question du groupe avant son
envoi ; cette dernière règle est conservée.
