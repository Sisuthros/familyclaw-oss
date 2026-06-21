# Discord-botin käyttöönotto-ohje

Tämä ohje neuvoo, kuinka luot Discord-botin ja konfiguroit sen FamilyClaw-järjestelmää varten.

## 1. Botin luonti Discord Developer Portalissa
1. Kirjaudu sisään [Discord Developer Portaliin](https://discord.com/developers/applications).
2. Napsauta oikeasta yläkulmasta **New Application**.
3. Anna sovellukselle nimi (esim. FamilyClaw) ja hyväksy ehdot. Napsauta **Create**.
4. Valitse vasemman reunan valikosta **Bot**.

## 2. Privileged Intents (Erityisluvat)
Jotta botti voi lukea viestien sisältöjä, sinun on kytkettävä päälle `MESSAGE_CONTENT`-intent. Ilman tätä luvallista ominaisuutta viestien sisältö on tyhjä, eikä botti pysty reagoimaan kanavan tapahtumiin.

1. Etsi Bot-sivulta osio **Privileged Gateway Intents**.
2. Kytke päälle **Message Content Intent**.
3. Tallenna muutokset (Save Changes).

*Huom: Yli 100 palvelimen botit vaativat Discordin erillisen tarkistuksen ja hyväksynnän tälle intentille.*

## 3. Botin kutsuminen palvelimelle
1. Siirry vasemman reunan valikosta kohtaan **OAuth2** -> **URL Generator**.
2. Valitse **Scopes**-kohdasta `bot`.
3. Valitse **Bot Permissions** -kohdasta:
   - `View Channels`
   - `Send Messages`
   - `Read Message History`
4. Kopioi sivun alareunaan generoitu URL-osoite (permissions-arvo on mukana bittimaskina, esim. `permissions=68608`).
5. Avaa kopioitu linkki selaimessa ja valitse palvelin, jolle haluat botin lisätä.

## 4. Kanava-ID:n selvittäminen
1. Avaa Discord-sovellus.
2. Siirry asetuksiin: **User Settings** -> **Advanced**.
3. Kytke päälle **Developer Mode**.
4. Napsauta hiiren oikealla painikkeella sitä tekstikanavaa, jota haluat botin käyttävän, ja valitse **Copy Channel ID** (Kopioi kanavan tunnus).

## 5. Konfigurointi
Botti tarvitsee toimiakseen kaksi ympäristömuuttujaa. 
**TÄRKEÄÄ:** Älä koskaan commitoi botin tokenia versionhallintaan! Lisää tiedosto `.env` `.gitignore`-tiedostoon.

Luo projektin juureen tiedosto nimeltä `.env` ja lisää sinne seuraavat rivit:
```env
DISCORD_BOT_TOKEN="Kopioi_botin_token_Bot_sivulta_tähän"
DISCORD_CHANNEL_ID="Kopioi_kanava_ID_tähän"
```

## 6. Vianetsintä

| Ongelma / Oire | Syy ja ratkaisu |
|----------------|-----------------|
| Tyhjät viestisisällöt | `MESSAGE_CONTENT`-intent puuttuu. Kytke se päälle Discord Developer Portalissa. |
| HTTP 401 Unauthorized | Väärä tai vanhentunut token. Tarkista `DISCORD_BOT_TOKEN`. |
| "Missing Access" tai HTTP 403 | Botti ei ole kyseisellä kanavalla tai siltä puuttuu luku-/kirjoitusoikeudet. |
| Ei tapahtumia (hiljaista) | Väärä kanava-ID muuttujassa `DISCORD_CHANNEL_ID`, tai botti on yhdistynyt mutta ei vastaanota guild-viestejä (botti tarvitsee `GUILDS`-intentin — ilman sitä guild jää pysyvästi *unavailable* eikä yksikään `MESSAGE_CREATE` saavu; tämä on sisäänrakennettu, ei käyttäjän säädettävä). |
