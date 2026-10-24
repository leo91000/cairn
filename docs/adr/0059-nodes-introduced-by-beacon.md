# Nodes mises en relation par Beacon, sans adresse

Date : 2026-10-24. Révise la conséquence de l’[ADR-0028](0028-autonomous-installations-behind-the-official-service.md)
selon laquelle des nodes supplémentaires doivent joindre le manager directement
et les disques ne transitent pas par le relais.

L’installation en une commande ne publie aucun port et son manager n’a qu’une
origine privée (`http://manager:4310`). Ajouter une machine exigeait pourtant une
adresse HTTPS directe du manager, avec un certificat reconnu par la node : un
proxy TLS, un VPN et une modification de `compose.json` qu’une réexécution de
l’installateur effaçait. La seconde machine était le point le plus difficile de
la prise en main.

Une node ajoutée sans adresse est **mise en relation** avec son manager par
Beacon, comme l’interface l’est avec l’installation
([ADR-0033](0033-direct-connection-with-relay-fallback.md)) :

- La commande d’installation vient de Beacon (`/install/node.sh`) avec l’ID de
  l’installation, le backend et l’**empreinte du certificat de node** du
  manager. Elle installe une **passerelle de node** (`cairn-node-gateway`,
  binaire statique construit avec Beacon et vérifié par SHA-256) qui expose
  `http://127.0.0.1:4319` sur la machine, puis exécute l’installateur habituel
  du manager à travers elle. Les runtimes Firecracker et natif, leur
  inscription, leurs mises à jour et la lecture des disques restent inchangés :
  ils voient une origine en boucle locale, déjà acceptée en HTTP.
- **Connexion directe.** La passerelle envoie une offre WebRTC complète, tous
  candidats compris (`POST /api/relay/{installation}/nodes/direct`). Beacon la
  transmet par le tunnel de l’installation (trame `NodeOffer`, protocole 5) et
  rend la réponse complète (`NodeAnswer`). La node et le manager échangent
  ensuite en UDP sans Beacon ; chaque connexion locale devient un DataChannel.
- **Secours par Beacon.** Quand les réseaux empêchent la connexion directe (UDP
  bloqué, NAT symétriques des deux côtés), chaque connexion locale devient un
  WebSocket vers Beacon (`/api/relay/{installation}/nodes/connect`), apparié à
  celui que l’installation ouvre pour ce canal (`NodeChannel`). Beacon recopie
  les octets : du TLS entre la passerelle et le manager.
- **Certificat de node.** Le manager garde une clé ECDSA persistante
  (`installation-relay/node-certificate.json`). Elle termine DTLS sur la
  connexion directe et TLS dans le canal de secours. La passerelle compare le
  certificat présenté à l’empreinte de sa commande : Beacon transmet l’offre, la
  réponse et les octets sans pouvoir lire les échanges ni se faire passer pour
  le manager.
- Le manager ne sert que les routes de node (`/internal/nodes/`,
  `/internal/node-restore/`, `/internal/node-workspace/`), chaque requête
  portant le code d’inscription, le jeton de node ou l’autorisation de disque
  qu’il contrôle.
- Le conteneur Firecracker a sa propre boucle locale : le démon de node y relie
  l’origine de la passerelle à son socket Unix, monté depuis l’hôte.

La passerelle tente la connexion directe à son démarrage et y attend jusqu’à
20 secondes avant de passer par Beacon. Après une coupure, elle retente aussitôt,
puis de plus en plus rarement (30 secondes à 10 minutes) si les réseaux la
bloquent. Après 15 secondes de connexion directe disponible, la passerelle ferme
progressivement ses connexions HTTP passées par Beacon : une réponse en cours,
y compris son corps diffusé en plusieurs parties, se termine intégralement avant
la fermeture. Le client se reconnecte en direct pour sa requête suivante, et les
disques ne restent pas sur Beacon. Une pause dans le flux ne suffit jamais à
considérer une réponse comme terminée.

Beacon borne les offres (30 par minute et par adresse, 32 en attente par
installation), l’ouverture des canaux par adresse et par minute, et leur nombre
simultané par installation (256) et par adresse (64). Le manager garde au plus
32 connexions directes de node. Les canaux de secours se ferment avec le tunnel
de l’installation ; les connexions directes n’en dépendent plus une fois
établies.

## Ce que voit Beacon

L’offre et la réponse : les adresses réseau des deux machines et leurs
empreintes DTLS, comme pour l’interface. La passerelle n’a aucun identifiant :
quiconque connaît l’ID de l’installation peut obtenir une réponse, donc les
adresses du manager, puis atteindre les routes de node, qui exigent toujours le
code, le jeton ou l’autorisation de la node. En secours, la taille et le rythme
d’un trafic chiffré, pas son contenu. Beacon sert aussi la commande et la
passerelle : un Beacon compromis pourrait les modifier, comme il peut déjà
modifier le code de l’interface. L’empreinte protège le trafic contre
l’interception, pas contre ce cas.

## Options écartées

- **Garder l’adresse directe obligatoire** : elle reste proposée, facultative,
  pour un réseau local ou un VPN qui garde tout le trafic hors de Beacon, même
  quand une connexion directe est impossible.
- **Tout faire passer par Beacon en clair** (première version de cette décision) :
  Beacon aurait lu les jetons de node, les comptes d’agent transmis et les
  disques, et porté tout leur débit.
- **Un relais TURN sur Beacon** : il relaierait le même DTLS, mais demande un
  serveur TURN et des identifiants, et l’UDP qu’il utilise est souvent ce qui
  est bloqué. Le WebSocket de secours passe partout où HTTPS sort.
- **Faire passer les nodes par le tunnel existant** : ses trames JSON en base64,
  bornées à 8 Mo, ne portent que l’API `/api/*`, et un gros transfert de disque
  y retarderait l’interface.
- **Une passerelle Python sans dépendance** : la bibliothèque standard n’a pas
  WebRTC.
- **Un client WebRTC dans chaque processus de node** : quatre clients Rust, deux
  superviseurs Python et `curl` utilisent l’origine du manager. Une origine en
  boucle locale les garde tous inchangés.

## Conséquences

- Le protocole du relais passe à la version 5. Une installation ou un Beacon plus
  ancien négocie la version 4 et refuse offres et canaux ; la passerelle affiche
  alors « offline or must be updated ».
- L’image de Beacon embarque la passerelle (`/usr/local/lib/cairn/cairn-node-gateway`,
  ou `CAIRN_NODE_GATEWAY`). Sans elle, `/install/node.sh` demande une adresse
  directe.
- La passerelle se met à jour en relançant la commande ; elle ne se met pas à
  jour seule.
- Un manager qui perd son certificat de node (données perdues) donne une
  nouvelle empreinte : ses nodes se réinstallent avec une nouvelle commande.
- La désinstallation d’une node retire la passerelle quand aucun autre runtime
  Cairn ne reste sur la machine.
- Une réexécution des installateurs de node rafraîchit leurs superviseurs sans
  réinscription ni redémarrage forcé.
