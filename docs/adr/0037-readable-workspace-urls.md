# Slugs lisibles pour les installations et les conversations

Date : 2026-10-12.

Les nouvelles URLs de l’interface utilisent un slug enregistré, par exemple
`/installations/quiet-cedar-1234abcd/chats/bright-otter-5678abcd`. Un slug associe
un adjectif, un nom et huit caractères hexadécimaux aléatoires. Une contrainte
unique et une nouvelle génération en cas de collision garantissent son unicité :
globale pour les installations, locale à chaque installation pour les conversations.
Renommer un élément ne change pas son slug. Les éléments existants en reçoivent
un à la migration, sans modifier leur identité ni leurs dates d’activité.

Les UUID restent les identifiants des données et des autorisations. Beacon résout
le slug d’une installation dans les seules installations accessibles au compte,
puis utilise son UUID pour le relais et la connexion directe. Les anciens liens
d’installation redirigent vers le slug en conservant le chemin, la requête et
le fragment. L’installation résout les slugs des conversations via un index ;
les accès par UUID restent acceptés. Les brouillons et la sélection d’une
conversation restent associés au UUID. Les API ne permettent pas de modifier
le slug. Connaître un slug ne donne aucun accès supplémentaire.

Les slugs des conversations restent conservés dans l’installation, conformément
à l’ADR-0028. Beacon ne reçoit aucun nouvel historique de conversation.

## Sous-domaines

Avec `CAIRN_BEACON_INSTALLATION_SUBDOMAINS=true`, l’interface utilise
`https://{slug}.cairn.build/chats/{conversation-slug}`. Chaque hôte sert la même
application Beacon ; les machines des installations restent derrière le relais
et la connexion directe décrits par l’ADR-0033. Le mode est désactivé par défaut
jusqu’à la préparation du DNS et du certificat wildcard.

Beacon accepte uniquement son hôte canonique et les slugs enregistrés des
installations réclamées, avec le port attendu. Sur un sous-domaine, les API
privées restent liées au UUID de cette installation. Les mutations requièrent
l’origine exacte de la requête et le jeton CSRF ; les lectures portant un en-tête
Origin sont également contrôlées. Un suffixe ressemblant, un sous-domaine imbriqué
ou une autre installation ne passent pas ce contrôle.

La session HttpOnly, Secure en HTTPS et SameSite=Lax est partagée via
`Domain=cairn.build`. Cette décision impose que tous les sous-domaines de ce
domaine soient de confiance : **ne pas y héberger ni déléguer des sites contrôlés
par des utilisateurs**. Ces hôtes ne constituent pas une frontière de sécurité
pour le cookie partagé. Les autorisations côté serveur restent obligatoires.
La migration efface d’abord l’ancien cookie limité à l’hôte puis réémet le même
jeton, avec l’échéance restante ; elle ne crée pas de session et ne renouvelle
pas la preuve d’identité. Le nom du cookie conserve le contrat Android.

Connexion, passkeys, callbacks OAuth, confirmations d’identité, paramètres du
compte et notifications restent sur l’origine canonique. Le retour vers une
installation vérifie son appartenance au compte et l’origine complète. Les
commandes d’installation et URLs MCP continuent d’utiliser cette origine.

Lors de l’ouverture d’un ancien lien, une iframe limitée à l’origine canonique
transfère les brouillons de cette installation dans une boîte temporaire du
localStorage de l’origine cible : WebKit ne conserve pas le sessionStorage d’une
iframe après sa navigation au premier plan. Un identifiant aléatoire isole chaque
transfert et les onglets ; il apparaît brièvement dans la requête de navigation,
puis est retiré avant le routage. Il ne contient ni brouillon ni identifiant de
session. Après vérification du compte et de l’installation, l’application restaure
les brouillons dans le sessionStorage et supprime la boîte. La boîte expire après
cinq minutes ; les boîtes expirées sont purgées lors de la prochaine visite.

L’échange vérifie la source, l’origine, le compte et l’installation ; il
conserve les brouillons d’origine et n’écrase pas les nouveaux, y compris les
brouillons explicitement vidés. Leur contenu ne
transite pas par le backend. Le thème et l’installation récemment sélectionnée
utilisent des cookies de préférence non sensibles pour traverser les origines.

Un retour au mode désactivé retire le cookie partagé au profit du cookie limité
à l’hôte, sans changer la session serveur. Les anciens liens par sous-domaine
redirigent vers les chemins canoniques si le DNS et le proxy wildcard sont
conservés. Les brouillons restent dans leur stockage navigateur d’origine.

## Mesures

Voir [les mesures locales](../READABLE-SLUG-MEASUREMENTS.md) pour le coût des
champs et index ajoutés au stockage. Ces mesures ne représentent pas la latence
réseau d’une installation en production.
