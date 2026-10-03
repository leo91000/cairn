# Transport Codex mesuré jusqu'au premier texte

Date : 2026-10-03. Statut : HTTP qualifié sur des tours natifs isolés ;
comparaison du parcours complet encore requise avant activation générale.

## Mesure et décision provisoire

L'envoi de la requête n'est pas le critère de gain : le préchauffage WebSocket
envoie le prompt et les outils avec `generate=false`, ce qui peut préparer un
cache serveur. Le premier fragment de texte reçu est capturé en continu avec
une horloge monotone, sans attendre la fin du processus ni les lots OTLP.

Codex 0.160.0 ouvre déjà le WebSocket en parallèle de la découverte des outils.
Les traces confirment un chevauchement de 232 à 1 003 ms. Le préchauffage du
prompt suit cette préparation. Le travail restant côté Leo comprend aussi
l'admission et la préparation de l'environnement avant l'ouverture du thread.

Premier comparatif sur un même guest préparé en production, compte ChatGPT et
modèle `gpt-6.1-sol/low`. Les modes sont alternés pour limiter le biais d'ordre.

| Comparatif | Essais par mode | WebSocket + préchauffage | HTTP sans préchauffage |
| --- | ---: | ---: | ---: |
| Nouveau processus, sans MCP | 4 | 5,72 s | 4,53 s |
| Service résident, vrai MCP, nouveau thread | 4 | 5,16 s | 4,10 s |
| Même service et thread repris, vrai MCP | 4 | 3,89 s | 2,75 s |

Ce sont des médianes du premier texte natif reçu. L'admission, la préparation
du guest et le rendu DOM sont exclus. Deux séries résidentes inversent l'ordre
des modes, chacune dans son propre guest préparé. Ces séries restent petites :
aucun p99 ni délai de trois secondes pour quatre arrivées n'est démontré. Sur
les reprises, le maximum HTTP atteint 5,84 s, contre 4,39 s pour WebSocket ; le
gain de médiane ne prouve pas un gain de latence maximale. HTTP change aussi
le transport : ce n'est pas un test qui isole uniquement le préchauffage.

Quatre autres tours HTTP, deux créations et deux reprises, exécutent chacun un
vrai outil et vérifient l'écriture/relecture d'un marqueur dans leur workspace.
Ils terminent avec le marqueur attendu et le même thread sur chaque reprise.
Les services privés sont arrêtés et les conversations de test supprimées après
acquittement de leur sauvegarde. Les données brutes restent hors de Git et
seront des assets de release si la qualification complète confirme un gain.

## Activation et retour arrière

`LEO_CODEX_TRANSPORT=http` sur le manager sélectionne un profil par thread.
Le défaut reste WebSocket. L'image doit fournir `APP_CODEX_VERSION` pour
conserver l'en-tête de version du vrai client natif ; sans cette information,
le manager journalise un avertissement et garde WebSocket.

Le profil HTTP utilise le nom `OpenAI`, l'authentification ChatGPT existante,
le backend natif par défaut, les métadonnées et les capacités de recherche.
Il ne définit ni URL de base alternative ni clé API. Les accès et le MCP
restent renouvelés à chaque tour ; aucune option ne duplique les arguments
CLI du processus résident.

Huit tours sur le vrai Codex qualifient les deux séquences HTTP → WebSocket →
HTTP et WebSocket → HTTP → WebSocket. Chaque séquence conserve exactement son
thread et les traces vérifient le transport effectivement utilisé. Deux essais
précédents avaient un tour normal terminé mais un marqueur différent de la
forme brute attendue ; leur réponse n'ayant pas été conservée, leur cause exacte
reste inconnue. Le répétiteur distingue désormais panne native, absence de
texte et différence de marqueur ; les huit derniers tours ont répondu avec
le marqueur brut attendu, sans normalisation nécessaire.

Retirer l'option, ou sélectionner `websocket`, transmet explicitement le
provider intégré `openai`, y compris pour une reprise d'un thread HTTP. Les
tests de conversation à froid et résidente vérifient la propagation du choix
avec renouvellement puis suppression des grants MCP.

Avant de choisir un défaut ou une release : comparer quatre arrivées
simultanées et des reprises depuis l'envoi UI jusqu'au premier texte reçu,
sur l'image exacte contenant ce changement.
